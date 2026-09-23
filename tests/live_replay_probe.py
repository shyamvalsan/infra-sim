#!/usr/bin/env python3
"""Replay a synthetic one-vnode archive into an isolated real Netdata receiver.

Raw acceptance: plugins.d bytes register a vnode with numeric CPU samples,
journal entries reach the real journal receiver unchanged, OTLP requests are
acknowledged and every recorded log record is stored, and exporter responses
match the captured bytes. Downstream Netdata states stay live and unasserted.
"""
import argparse
import collections
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import uuid
from urllib.request import urlopen

from live_agent_smoke import ROOT, run, wait_for

MODES = ('metrics', 'logs', 'otlp', 'exporters')


def frames(directory):
    limit = struct.unpack('<Q', (directory / 'committed').read_bytes())[0]
    with (directory / 'events.bin').open('rb') as source:
        assert source.read(8) == b'ISIMREC1'
        while source.tell() < limit:
            meta_size, payload_size = struct.unpack('<IQ', source.read(12))
            metadata = json.loads(source.read(meta_size))
            payload = source.read(payload_size)
            assert len(payload) == payload_size
            yield metadata, payload
        assert source.tell() == limit


def journal_entries(stream):
    """Parse Journal Export Format, including binary-form fields."""
    entries, fields, at = [], {}, 0
    while at < len(stream):
        end = stream.index(b'\n', at)
        line = stream[at:end]
        at = end + 1
        if not line:
            if fields:
                entries.append(fields)
            fields = {}
            continue
        if b'=' in line:
            name, value = line.split(b'=', 1)
        else:
            size = struct.unpack('<Q', stream[at:at + 8])[0]
            name, value = line, stream[at + 8:at + 8 + size]
            at += 8 + size + 1
        fields[name.decode()] = value.decode()
    assert not fields, 'journal stream ends inside an entry'
    return entries


def protobuf(message):
    """Yield (field, wire type, value) for one protobuf message."""
    at = 0

    def varint():
        nonlocal at
        shift = result = 0
        while True:
            byte = message[at]
            at += 1
            result |= (byte & 0x7F) << shift
            if byte < 0x80:
                return result
            shift += 7

    while at < len(message):
        key = varint()
        field, wire = key >> 3, key & 7
        if wire == 0:
            yield field, wire, varint()
        elif wire == 1:
            yield field, wire, message[at:at + 8]
            at += 8
        elif wire == 2:
            size = varint()
            yield field, wire, message[at:at + size]
            at += size
        elif wire == 5:
            yield field, wire, message[at:at + 4]
            at += 4
        else:
            raise ValueError(f'unsupported protobuf wire type {wire}')


def otlp_log_counts(request):
    """Count LogRecords per (service.namespace, service.name) in one export request."""
    counts = collections.Counter()
    for field, _, resource_logs in protobuf(request):
        if field != 1:
            continue
        attributes, records = {}, 0
        for sub, _, value in protobuf(resource_logs):
            if sub == 1:  # Resource
                for attr_field, _, attribute in protobuf(value):
                    if attr_field != 1:
                        continue
                    parts = dict((f, v) for f, _, v in protobuf(attribute))
                    string = dict((f, v) for f, _, v in protobuf(parts.get(2, b''))).get(1)
                    if isinstance(string, bytes):
                        attributes[parts[1].decode()] = string.decode()
            elif sub == 2:  # ScopeLogs
                records += sum(1 for f, _, _ in protobuf(value) if f == 2)
        counts[(attributes.get('service.namespace'), attributes.get('service.name'))] += records
    return counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--plugin', type=Path,
                        help='replay with this build instead of the archived executable')
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise SystemExit('run with sudo; the disposable receiver writes journal files')
    archive = args.archive.resolve()
    status = json.loads(run(str(archive / 'infra-sim.plugin'), '--recording-status', str(archive / 'recording')))
    assert status['finalized'] and not status['incomplete'], status
    routes = collections.defaultdict(list)
    journals = collections.defaultdict(bytearray)
    otlp_records = collections.Counter()
    spans = 0
    hostnames = set()
    for metadata, payload in frames(archive / 'recording'):
        kind = metadata['kind']
        if kind == 'exporter':
            routes[metadata['target']].append(payload)
        elif kind == 'journal':
            journals[metadata['target']] += payload
        elif kind == 'otlp_logs':
            otlp_records += otlp_log_counts(payload)
        elif kind == 'otlp_traces':
            spans += 1
        elif kind == 'metrics':
            for line in payload.split(b'\n'):
                if line.startswith(b'HOST_DEFINE '):
                    hostnames.add(shlex.split(line.decode())[2])
    assert routes and journals and otlp_records and spans and hostnames, \
        'probe requires recorded metrics, application, journal and OTLP output'
    assert len(hostnames) <= 5, 'local validation is capped at five vnodes'
    expected_journal = {
        target: collections.Counter((e['__REALTIME_TIMESTAMP'], e['MESSAGE'])
                                    for e in journal_entries(bytes(stream)))
        for target, stream in journals.items()}
    name = 'infra-sim-replay-' + uuid.uuid4().hex[:10]
    work = Path(tempfile.mkdtemp(prefix='infra-sim-replay-'))
    work.chmod(0o755)
    recording = work / 'record'
    shutil.copytree(archive, recording)
    # A finalized archive must replay from read-only root-owned media, so the
    # copy keeps no writable file for the receiver's netdata UID.
    for path in [recording, *recording.rglob('*')]:
        if path.is_symlink():
            raise RuntimeError('probe refuses symlinked archive artifacts')
        path.chmod(0o755 if path.is_dir() or path.name in ('infra-sim.plugin', 'replay.plugin') else 0o644)
    if args.plugin:
        shutil.copy2(args.plugin.resolve(), recording / 'replay.plugin')
        executable = '/record/replay.plugin'
    else:
        executable = '/record/infra-sim.plugin'
    plugins = work / 'plugins'
    plugins.mkdir()
    journal = work / 'journal'
    journal.mkdir()
    config = (ROOT / 'docker/netdata.conf.template').read_text().replace('__SIM_NAME__', name)
    config = '\n'.join(line for line in config.splitlines() if '__SIM_' not in line)
    (work / 'netdata.conf').write_text(config)
    start = int(time.time()) + 20
    wrapper = plugins / 'replay.plugin'
    wrapper.write_text(f'''#!/bin/sh
if [ -e /tmp/replay-metrics-started ]; then echo DISABLE; exit 0; fi
touch /tmp/replay-metrics-started
{executable} --replay-recording /record/recording --replay-start-at {start} "$@" 2>/tmp/replay-metrics.out
result=$?
printf '%s\\n' "$result" > /tmp/replay-metrics.exit
exit "$result"
''')
    wrapper.chmod(0o755)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]

    def exists():
        return subprocess.run(['docker', 'container', 'inspect', name],
                              capture_output=True).returncode == 0

    def exit_status(mode):
        result = subprocess.run(['docker', 'exec', name, 'cat', f'/tmp/replay-{mode}.exit'],
                                capture_output=True, text=True)
        return None if result.returncode else result.stdout.strip()

    def failed_producers():
        # Surface a producer failure at once instead of waiting out a timeout.
        for mode in MODES:
            code = exit_status(mode)
            if code not in (None, '0'):
                detail = subprocess.run(['docker', 'exec', name, 'cat', f'/tmp/replay-{mode}.out'],
                                        capture_output=True, text=True).stdout
                raise AssertionError(f'{mode} replay exited {code}: {detail.strip()}')

    def watched(probe):
        def check():
            failed_producers()
            return probe()
        return check

    try:
        run('docker', 'run', '-d', '--name', name, '-p', f'127.0.0.1:{port}:19999',
            '-v', f'{plugins}:/etc/netdata/custom-plugins.d:ro',
            '-v', f'{recording}:/record:ro',
            '-v', f'{journal}:/var/log/journal/remote',
            '-v', f'{work / "netdata.conf"}:/etc/netdata/netdata.conf:ro',
            '--entrypoint', '/usr/sbin/run.sh', 'infra-sim:latest')
        extra = {'logs': ['--logs', '--journal-dir', '/var/log/journal/remote'],
                 'otlp': ['--otlp', '--otlp-endpoint', '127.0.0.1:4317'],
                 'exporters': ['--exporters', '--exporter-port', '19998']}
        for mode, options in extra.items():
            command = [executable, '--replay-recording', '/record/recording',
                       '--replay-start-at', str(start), *options]
            shell = f'{shlex.join(command)} >/tmp/replay-{mode}.out 2>&1; printf "%s\\n" "$?" >/tmp/replay-{mode}.exit'
            run('docker', 'exec', '-d', name, 'sh', '-c', shell)

        def query(path):
            with urlopen(f'http://127.0.0.1:{port}' + path, timeout=5) as response:
                return json.load(response)

        for hostname in sorted(hostnames):
            wait_for(f'replayed vnode {hostname} registered by real plugins.d', watched(lambda: any(
                node.get('nm') == hostname for node in query('/api/v3/nodes').get('nodes', []))))
            wait_for(f'replayed CPU samples for {hostname} stored by Netdata', watched(lambda: any(
                any(isinstance(value, (int, float)) for value in row[1:])
                for row in query(f'/host/{hostname}/api/v1/data?chart=system.cpu&after=-60').get('data', []))))
        # Consume each captured response once; go.d is deliberately not pointed
        # at the replay port so it cannot race this byte comparison.
        for route, payloads in routes.items():
            for expected in payloads:
                failed_producers()
                result = subprocess.run(['docker', 'exec', name, 'curl', '-fsS', '--max-time', '180',
                                         'http://127.0.0.1:19998' + route], capture_output=True, timeout=190)
                assert result.returncode == 0, result.stderr.decode(errors='replace')
                assert result.stdout == expected, f'exporter replay bytes differ for {route}'
        print(f'PASS: all {sum(map(len, routes.values()))} replayed exporter responses match captured bytes', flush=True)

        wait_for('all replay producers completed successfully', watched(
            lambda: all(exit_status(mode) == '0' for mode in MODES)), timeout=300)
        for target, expected in expected_journal.items():
            output = run('docker', 'exec', name, 'journalctl', '--file=/var/log/journal/remote/' + target,
                         '-o', 'json', '--no-pager', '--output-fields=__REALTIME_TIMESTAMP,MESSAGE')
            stored = collections.Counter((e['__REALTIME_TIMESTAMP'], e['MESSAGE'])
                                         for e in map(json.loads, output.splitlines()))
            assert stored == expected, f'{target}: stored journal entries differ from the recording'
        print(f'PASS: real journal receiver stored all {sum(map(sum, [c.values() for c in expected_journal.values()]))} '
              'recorded entries with identical timestamps and messages', flush=True)
        for (namespace, service), count in sorted(otlp_records.items()):
            output = run('docker', 'exec', name, '/usr/libexec/netdata/plugins.d/otel-plugin', 'logs',
                         '--wal-dir', '/var/log/netdata/otel/v2/logs/wal',
                         '--sfst-dir', '/var/log/netdata/otel/v2/logs/index',
                         '--name', service, '--namespace', namespace, '--limit', str(count + 100))
            stored = sum(1 for line in output.splitlines() if line.strip())
            # A fresh receiver: exactly the recorded records, none lost or duplicated.
            assert stored == count, f'{namespace}/{service}: {stored} stored records for {count} recorded'
            print(f'PASS: OTLP {namespace}/{service}: all {count} recorded log records stored once', flush=True)
        traces = run('docker', 'exec', name, 'find', '/var/log/netdata/otel/v2/traces', '-type', 'f', '-size', '+0c')
        assert traces.strip(), 'no stored OTLP traces'
        print(f'PASS: receiver acknowledged {spans} trace requests without rejection and stored them', flush=True)
    finally:
        if exists():
            logs = subprocess.run(['docker', 'logs', name], capture_output=True, text=True)
            (work / 'netdata.log').write_text(logs.stdout + logs.stderr)
            run('docker', 'rm', '-f', name)
        print(f'Replay diagnostics retained at {work}', flush=True)


if __name__ == '__main__':
    main()
