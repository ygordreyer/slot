import argparse
import base64
import contextlib
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import shlex
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zlib

PATH = Path(__file__).with_name('slopctl')
loader = importlib.machinery.SourceFileLoader('slopctl', str(PATH))
spec = importlib.util.spec_from_loader(loader.name, loader)
slop = importlib.util.module_from_spec(spec)
loader.exec_module(slop)

FAKE = r'''#!/usr/bin/env python3
import base64, json, os, pathlib, subprocess, sys
args = sys.argv[1:]
record = {'argv': args}
if 'push' in args:
    index = args.index('push')
    record['bytes'] = base64.b64encode(pathlib.Path(args[index + 1]).read_bytes()).decode()
with open(os.environ['FAKE_ADB_LOG'], 'a') as f:
    f.write(json.dumps(record) + '\n')
config = json.loads(pathlib.Path(os.environ['FAKE_ADB_CONFIG']).read_text())
files_path = pathlib.Path(os.environ['FAKE_ADB_CONFIG'] + '.files')
files = json.loads(files_path.read_text()) if files_path.exists() else {}
if 'push' in args:
    files[args[-1]] = record['bytes']
    files_path.write_text(json.dumps(files))
if args and args[0] == '-s': args = args[2:]
if args[:1] == ['devices']:
    print(config.get('devices', 'List of devices attached\r\nSLOP\tdevice usb:1-1 product:BaseOS\r\n'), end='')
elif args[:1] == ['shell']:
    command = args[1]
    for remote, encoded in files.items():
        if remote.startswith('/tmp/slopctl-script-') and 'sh ' + remote in command:
            command = base64.b64decode(encoded).decode()
            break
    if config.get('execute_shell'):
        sys.exit(subprocess.run(['sh', '-c', command]).returncode)
    output = 'pushed' if 'mv -f' in command else ''
    rc = 0
    for rule_index, rule in enumerate(config.get('rules', [])):
        if rule['contains'] in command:
            answers = rule.get('outputs')
            if answers:
                key = 'count:' + str(rule_index)
                count = files.get(key, 0)
                output = answers[min(count, len(answers) - 1)]
                files[key] = count + 1
                files_path.write_text(json.dumps(files))
            else:
                output = rule.get('output', '')
            rc = rule.get('rc', 0)
            break
    sys.stdout.write(output.replace('\n', '\r\n') + '\r\n__slopctl_rc=' + str(rc) + '\r\n')
elif args[:1] == ['pull']:
    pathlib.Path(args[2]).write_bytes(base64.b64decode(config['pull_bytes']))
else:
    print('OK')
'''


FAKE_SSH = r'''#!/usr/bin/env python3
import base64, json, os, pathlib, subprocess, sys
args = sys.argv[1:]
payload = sys.stdin.buffer.read()
record = {'argv': args, 'bytes': base64.b64encode(payload).decode()}
with open(os.environ['FAKE_SSH_LOG'], 'a') as f:
    f.write(json.dumps(record) + '\n')
config = json.loads(pathlib.Path(os.environ['FAKE_SSH_CONFIG']).read_text())
while args[:1] == ['-o']: args = args[2:]
host, command = args[0], ' '.join(args[1:])
if command == 'true': sys.exit(config.get('probe_rc', 0))
if config.get('execute_shell'):
    sys.exit(subprocess.run(['sh', '-c', command], input=payload).returncode)
for rule in config.get('rules', []):
    if rule['contains'] in command:
        if 'bytes' in rule:
            sys.stdout.buffer.write(base64.b64decode(rule['bytes']))
        else:
            sys.stdout.write(rule.get('output', ''))
        sys.stderr.write(rule.get('stderr', ''))
        sys.exit(rule.get('rc', 0))
'''


def cap(*codes):
    mask = sum(1 << code for code in codes)
    words = []
    while mask:
        words.append(f'{mask & ((1 << 64) - 1):x}')
        mask >>= 64
    return ' '.join(reversed(words)) or '0'


INPUT = ('I: Bus=0019\nN: Name="axp2202-pek"\nH: Handlers=event0\nB: KEY=' + cap(116) +
         '\n\nI: Bus=0019\nN: Name="gpio-keys"\nH: Handlers=js0 event3\nB: KEY=' +
         cap(0x130, 0x138, 115, 114) + '\nB: ABS=' + cap(16, 17) + '\n\n')


class TestEnvironment(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        fake = self.root / 'adb'
        fake.write_text(FAKE)
        fake.chmod(0o755)
        fake_ssh = self.root / 'ssh'
        fake_ssh.write_text(FAKE_SSH)
        fake_ssh.chmod(0o755)
        self.ssh_log = self.root / 'ssh.jsonl'
        self.ssh_config = self.root / 'ssh-config.json'
        self.ssh_config.write_text('{}')
        self.log = self.root / 'calls.jsonl'
        self.config = self.root / 'config.json'
        self.configure()
        self.env = patch.dict(os.environ, {'PATH': str(self.root) + os.pathsep + os.environ['PATH'],
                                          'FAKE_ADB_LOG': str(self.log), 'FAKE_ADB_CONFIG': str(self.config),
                                          'FAKE_SSH_LOG': str(self.ssh_log),
                                          'FAKE_SSH_CONFIG': str(self.ssh_config),
                                          'SLOPCTL_TRANSPORT': 'adb', 'SLOPCTL_HOST': 'slop',
                                          'SLOPCTL_SRC': ''})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.adb = slop.Adb()
        self.adb.connect()

    def configure(self, **values):
        self.config.write_text(json.dumps(values))

    def records(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def cli(self, *args):
        return subprocess.run([str(PATH), *args], capture_output=True, text=True, timeout=30)


class FakeAdbTests(TestEnvironment):
    def test_crlf_and_sentinel(self):
        self.configure(rules=[{'contains': 'hello', 'output': 'hello\nworld\n'}])
        self.assertEqual(self.adb.shell('echo hello'), 'hello\nworld\n')
        self.assertEqual(self.records()[-1]['argv'][:3], ['-s', 'SLOP', 'shell'])

    def test_nonzero_remote_rc_is_error(self):
        self.configure(rules=[{'contains': 'bad', 'output': 'failed', 'rc': 7}])
        with self.assertRaisesRegex(slop.Error, 'exited 7: failed'):
            self.adb.shell('bad')
        result = self.cli('shell', 'bad')
        self.assertEqual(result.returncode, 1)
        self.assertIn('exited 7', result.stderr)
        self.assertNotIn('Traceback', result.stderr)

    def test_long_script_is_transferred_not_in_shell_argv(self):
        self.configure(rules=[{'contains': 'LONG_BODY', 'output': 'done'}])
        self.assertEqual(self.adb.shell('echo LONG_BODY; ' + '#' * 5000), 'done')
        records = self.records()
        self.assertTrue(any('push' in r['argv'] for r in records))
        self.assertTrue(all(len(r['argv'][-1].encode()) <= 2000 for r in records if 'shell' in r['argv']))
        self.assertIn('rm -f /tmp/slopctl-script-', records[-1]['argv'][-1])

    def test_exit_in_command_still_has_sentinel(self):
        self.configure(execute_shell=True)
        self.assertEqual(self.adb.shell('printf ok'), 'ok')
        with self.assertRaisesRegex(slop.Error, 'exited 12'):
            self.adb.shell('exit 12')

    def test_device_arguments_are_quoted(self):
        self.configure(execute_shell=True)
        result = self.cli('shell', 'printf', '%s', 'a;$(false) \' b')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "a;$(false) ' b")

    def test_single_shell_argument_is_a_shell_command(self):
        self.configure(execute_shell=True)
        result = self.cli('shell', 'printf "%s" one | tr o O')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, 'One')

    def test_no_device_has_cable_hint(self):
        self.configure(devices='List of devices attached\r\n\r\n')
        result = self.cli('status')
        self.assertEqual(result.returncode, 1)
        self.assertIn('BEFORE powering on', result.stderr)
        self.assertIn('press POWER', result.stderr)
        self.assertEqual(len(result.stderr.splitlines()), 1)
        self.assertNotIn('Traceback', result.stderr)

    def test_tcp_device_not_selected(self):
        self.configure(devices='List of devices attached\n192.0.2.1:5555\tdevice\n')
        with self.assertRaisesRegex(slop.Error, 'No USB'):
            self.adb.connect()

    def make_source(self, contents):
        src = self.root / 'dist'
        for name, data in contents.items():
            path = src / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        return src

    def deploy(self, src, hashes, dry=False, only='all', algorithm='sha256sum'):
        inventory = 'algorithm=' + algorithm + '\n' + ''.join(f'{i}={h}\n' for i, h in enumerate(hashes))
        self.configure(rules=[{'contains': 'algorithm=%s', 'output': inventory}])
        args = argparse.Namespace(src=src, only=only, dry_run=dry, no_restart=True)
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            slop.deploy(self.adb, args)
        return output.getvalue()

    def test_config_recheck_at_write_boundary(self):
        self.configure(execute_shell=True)
        target, temp = self.root / 'config', self.root / 'temp'
        target.write_text('owner edits')
        temp.write_text('incoming')
        self.assertEqual(self.adb.shell(slop.atomic_command(temp, target, preserve=True)).strip(), 'skipped')
        self.assertEqual(target.read_text(), 'owner edits')
        self.assertFalse(temp.exists())
        target.unlink()
        temp.write_text('incoming')
        self.assertEqual(self.adb.shell(slop.atomic_command(temp, target, preserve=True)).strip(), 'pushed')
        self.assertEqual(target.read_text(), 'incoming')

    def test_restart_uses_term_waits_for_exit_and_new_pid(self):
        self.configure(rules=[{'contains': 'n=0\nwhile :', 'output': '200'},
                              {'contains': 'slot_pids', 'output': '100'}])
        with contextlib.redirect_stdout(io.StringIO()) as output:
            slop.lifecycle(self.adb, 'restart')
        self.assertIn('slot pid: 200', output.getvalue())
        commands = [r['argv'][-1] for r in self.records()]
        self.assertTrue(any('kill -TERM 100' in command for command in commands))
        self.assertTrue(any('[ -d /proc/100 ]' in command and 'term_timeout' in command for command in commands))
        self.assertFalse(any('KILL' in command for command in commands))

    def test_stop_sets_hold_before_term_and_start_clears_it(self):
        self.configure(rules=[{'contains': 'n=0\nwhile :', 'output': '200'},
                              {'contains': 'slot_pids', 'output': '100'}])
        with contextlib.redirect_stdout(io.StringIO()):
            slop.lifecycle(self.adb, 'stop')
            slop.lifecycle(self.adb, 'start')
        commands = [r['argv'][-1] for r in self.records()]
        hold_index = next(i for i, c in enumerate(commands) if ': > /run/slop-hold' in c)
        term_index = next(i for i, c in enumerate(commands) if 'kill -TERM' in c)
        clear_index = next(i for i, c in enumerate(commands) if 'rm -f /run/slop-hold' in c)
        self.assertLess(hold_index, term_index)
        self.assertGreater(clear_index, term_index)

    def test_stop_timeout_surfaces_error(self):
        self.configure(rules=[{'contains': 'term_timeout', 'output': 'term_timeout'},
                              {'contains': 'slot_pids', 'output': '100'}])
        with self.assertRaisesRegex(slop.Error, 'did not exit'):
            slop.lifecycle(self.adb, 'stop')

    def test_deploy_detects_changes_and_atomic_replace(self):
        src = self.make_source({'System/slot': b'new', 'Shaders/a.glsl': b'unchanged'})
        output = self.deploy(src, [hashlib.sha256(b'unchanged').hexdigest(), hashlib.sha256(b'old').hexdigest()])
        self.assertIn('pushed=1 unchanged=1 skipped=0', output)
        records = self.records()
        pushes = [r for r in records if 'push' in r['argv']]
        self.assertEqual(len(pushes), 1)
        self.assertEqual(base64.b64decode(pushes[0]['bytes']), b'new')
        temp = pushes[0]['argv'][-1]
        self.assertTrue(temp.startswith('/mnt/sdcard/System/.slopctl-'))
        command = next(r['argv'][-1] for r in records if 'mv -f' in r['argv'][-1])
        self.assertIn(f'mv -f {temp} /mnt/sdcard/System/slot', command)
        inventories = [r for r in records if 'algorithm=%s' in r['argv'][-1]]
        self.assertEqual(len(inventories), 1)
        self.assertIn('sync', records[-1]['argv'][-1])

    def test_deploy_never_overwrites_config_but_updates_examples(self):
        src = self.make_source({'Config/new.toml': b'initial', 'Config/user.toml': b'bad',
                                'Config/wifi.toml.example': b'example'})
        output = self.deploy(src, ['-', 'a' * 64, 'b' * 64])
        self.assertIn('pushed=2 unchanged=0 skipped=1', output)
        pushes = [r['argv'] for r in self.records() if 'push' in r['argv']]
        self.assertEqual({Path(r[-2]).name for r in pushes}, {'new.toml', 'wifi.toml.example'})
        commands = [r['argv'][-1] for r in self.records() if 'mv -f' in r['argv'][-1]]
        self.assertTrue(any('if [ -e /mnt/sdcard/Config/new.toml ]' in c for c in commands))

    def test_protected_dirs_refused_and_excluded(self):
        for directory in ('Games', 'Saves', 'States', 'BIOS', 'games'):
            with self.subTest(directory=directory):
                with self.assertRaisesRegex(slop.Error, 'Protected'):
                    slop.safe_relative(directory + '/file')
        src = self.make_source({'Games/cart.rom': b'rom', 'Saves/cart.sav': b'save',
                                'States/cart.state': b'state', 'BIOS/gba.bin': b'bios', 'System/slot': b'new'})
        output = self.deploy(src, ['-'])
        self.assertIn('pushed=1 unchanged=0 skipped=4', output)
        self.assertEqual(len([r for r in self.records() if 'push' in r['argv']]), 1)
        with self.assertRaisesRegex(slop.Error, 'Protected'):
            slop.deploy_files(src / 'Games', 'all')

    def test_dry_run_has_no_mutations(self):
        src = self.make_source({'System/slot': b'new'})
        output = self.deploy(src, ['-'], dry=True)
        self.assertIn('would push: System/slot', output)
        self.assertFalse(any('push' in r['argv'] or 'mv -f' in r['argv'][-1] or 'sync' in r['argv'][-1]
                             for r in self.records()))

    def test_missing_hash_tools_fails_before_payload_push(self):
        src = self.make_source({'System/slot': b'new'})
        self.configure(rules=[{'contains': 'algorithm=%s', 'output': 'No sha256sum or md5sum available', 'rc': 1}])
        args = argparse.Namespace(src=src, only='all', dry_run=False, no_restart=True)
        with self.assertRaisesRegex(slop.Error, 'No sha256sum'):
            slop.deploy(self.adb, args)
        self.assertFalse(any('push' in r['argv'] for r in self.records()))

    def test_md5_hash_fallback(self):
        src = self.make_source({'System/slot': b'new'})
        output = self.deploy(src, [hashlib.md5(b'new').hexdigest()], algorithm='md5sum')
        self.assertIn('unchanged=1', output)
        self.assertFalse(any('push' in r['argv'] for r in self.records()))

    def test_symlink_and_traversal_refusal(self):
        src = self.make_source({'System/slot': b'new'})
        (src / 'System/link').symlink_to(src / 'System/slot')
        with self.assertRaisesRegex(slop.Error, 'Symlinks'):
            slop.deploy_files(src, 'all')
        for path in ('/System/slot', 'System/../Saves/file', 'Config/x\ny'):
            with self.assertRaises(slop.Error):
                slop.safe_relative(path)

    def test_event_node_discovery_uses_capabilities_and_names(self):
        self.configure(rules=[{'contains': '/proc/bus/input/devices', 'output': INPUT}])
        listing = self.adb.shell('cat /proc/bus/input/devices')
        self.assertEqual(slop.discover_event(listing, 'A'), ('/dev/input/event3', 'gpio-keys'))
        self.assertEqual(slop.discover_event(listing, 'DOWN'), ('/dev/input/event3', 'gpio-keys'))
        self.assertEqual(slop.discover_event(listing, 'POWER'), ('/dev/input/event0', 'axp2202-pek'))
        with self.assertRaises(slop.Error):
            slop.discover_event(listing, 'L2')

    def test_press_pushes_exact_binary_and_separate_writes(self):
        self.configure(rules=[{'contains': '/proc/bus/input/devices', 'output': INPUT}])
        slop.inject(self.adb, [('A', 100)])
        pushes = [r for r in self.records() if 'push' in r['argv']]
        expected_down = bytes.fromhex('000000000000000000000000000000000100300101000000') + bytes(24)
        expected_up = bytes.fromhex('000000000000000000000000000000000100300100000000') + bytes(24)
        self.assertEqual(base64.b64decode(pushes[0]['bytes']), expected_down)
        self.assertEqual(base64.b64decode(pushes[1]['bytes']), expected_up)
        command = next(r['argv'][-1] for r in self.records() if 'usleep 100000' in r['argv'][-1])
        self.assertIn('> /dev/input/event3', command)
        self.assertLess(command.index('/0-1'), command.index('usleep 100000'))
        self.assertGreater(command.rindex('/0-0'), command.index('usleep 100000'))

    def test_shot_pulls_visible_page(self):
        self.configure(rules=[{'contains': 'virtual_size', 'output':
                               'virtual_size=2,4\nbits_per_pixel=32\nstride=8\npan=0,2\nmodes=U:2x2p-60\n'}],
                       pull_bytes=base64.b64encode(bytes(16)).decode())
        out = self.root / 'shot.png'
        with contextlib.redirect_stdout(io.StringIO()):
            slop.shot(self.adb, argparse.Namespace(out=out, rotate=0))
        self.assertTrue(out.read_bytes().startswith(b'\x89PNG'))
        commands = [r['argv'][-1] for r in self.records()]
        self.assertTrue(any('bs=8 skip=2 count=2' in c for c in commands))
        self.assertTrue(any('pull' in r['argv'] for r in self.records()))
        self.assertFalse(any('exec-out' in r['argv'] for r in self.records()))

    def test_help_and_duration_bounds(self):
        result = self.cli('--help')
        self.assertEqual(result.returncode, 0)
        self.assertIn('BEFORE powering on', result.stdout)
        self.assertEqual(self.cli('wait', '--timeout', '31').returncode, 2)
        self.assertEqual(self.cli('press', 'A', '--hold', '-1').returncode, 2)


class LifecycleTests(unittest.TestCase):
    def test_save_ack_reads_either_log_when_the_other_is_missing(self):
        for name in ('slot.log', 'slot.log.1'):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                saved = 'slot: sigterm: pid 100: saved, exiting'
                (root / name).write_text(saved + '\n')
                command = slop.SAVE_ACK_FUNCTION.replace('/mnt/sdcard', slop.q(root)) + 'save_ack 100 101'
                result = subprocess.run(['sh', '-c', command], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                with contextlib.redirect_stdout(io.StringIO()) as output, \
                        contextlib.redirect_stderr(io.StringIO()) as errors:
                    slop.save_acknowledgements(result.stdout, ['100', '101'])
                self.assertEqual(output.getvalue(), 'slot pid 100: saved\n')
                self.assertIn('slot pid 101: no save acknowledgement', errors.getvalue())
                self.assertNotIn('slot pid 100', errors.getvalue())

    def test_restart_reports_save_acknowledgements(self):
        for outcome, expected, warning in [
                ('saved, exiting', 'slot pid 100: saved', False),
                ('save incomplete, exiting', 'save incomplete', True),
                ('', 'no save acknowledgement; this slot build may predate the SIGTERM handler', True)]:
            with self.subTest(outcome=outcome):
                line = f'slot: sigterm: pid 100: {outcome}\n' if outcome else ''
                device = slop.Ssh()
                with patch.object(device, 'shell', side_effect=['', '100', '', line, '200']) as shell, \
                        contextlib.redirect_stdout(io.StringIO()) as output, \
                        contextlib.redirect_stderr(io.StringIO()) as errors:
                    slop.lifecycle(device, 'restart')
                self.assertIn(expected, errors.getvalue() if warning else output.getvalue())
                self.assertEqual(bool(errors.getvalue()), warning)
                self.assertIn('slot pid: 200', output.getvalue())
                wait = shell.call_args_list[3].args[0]
                self.assertIn('grep -h -F "slot: sigterm: pid $pid:"', wait)
                self.assertIn('/mnt/sdcard/slot.log /mnt/sdcard/slot.log.1', wait)
                self.assertIn('save_ack 100', wait)

    def run_stop_shell(self, continuous):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scans, signals, sleeps, hold = (root / name for name in ('scans', 'signals', 'sleeps', 'hold'))
            # Execute the actual device poll with deterministic pids and a clock advanced by sleep.
            functions = f'''
slot_pids() {{
    n=$(cat {slop.q(scans)} 2>/dev/null || echo 0)
    echo $((n+1)) > {slop.q(scans)}
    {'echo $((424242+n))' if continuous else 'case "$n" in 1) echo 424242 ;; esac'}
}}
kill() {{ printf '%s\\n' "$*" >> {slop.q(signals)}; }}
sleep() {{ echo slept >> {slop.q(sleeps)}; }}
'''
            calls = []

            class Device:
                def shell(self, command, timeout=30):
                    calls.append((command, timeout))
                    if ': > ' + slop.q(hold) in command:
                        hold.write_text('')
                        return ''
                    if command == functions + 'slot_pids':
                        return ''
                    result = subprocess.run(['sh', '-c', command], capture_output=True, text=True, timeout=5)
                    if result.returncode:
                        raise slop.Error(result.stdout.strip())
                    return result.stdout

            with patch.object(slop, 'PID_FUNCTION', functions), patch.object(slop, 'HOLD', str(hold)), \
                    contextlib.redirect_stdout(io.StringIO()) as output, \
                    contextlib.redirect_stderr(io.StringIO()) as errors:
                if continuous:
                    with self.assertRaisesRegex(slop.Error, 'within 15 s; hold remains set'):
                        slop.lifecycle(Device(), 'stop')
                else:
                    slop.lifecycle(Device(), 'stop')
            self.assertTrue(hold.exists())
            quiet_calls = [call for call in calls if 'quiet=0' in call[0]]
            self.assertEqual(len(quiet_calls), 1)
            self.assertLessEqual(quiet_calls[0][1], 30)
            signal_lines = signals.read_text().splitlines()
            if continuous:
                self.assertEqual(int(scans.read_text()), 16)
                self.assertEqual(len(sleeps.read_text().splitlines()), 15)
                self.assertEqual(len(signal_lines), 16)
                self.assertNotIn('Frontend stopped', output.getvalue())
            else:
                self.assertEqual(int(scans.read_text()), 4)
                self.assertEqual(signal_lines, ['-TERM 424242'])
                self.assertEqual(output.getvalue(), 'Frontend stopped; hold set.\n')
                self.assertIn('slot pid 424242: no save acknowledgement', errors.getvalue())

    def test_stop_terminates_a_late_pid_before_reporting_success(self):
        self.run_stop_shell(continuous=False)

    def test_stop_fails_when_pids_keep_appearing_for_the_bound(self):
        self.run_stop_shell(continuous=True)


class FakeSshTests(TestEnvironment):
    def configure_ssh(self, **values):
        self.ssh_config.write_text(json.dumps(values))

    def ssh_records(self):
        return [json.loads(line) for line in self.ssh_log.read_text().splitlines()]

    def ssh_commands(self):
        return [shlex.split(r['argv'][-1])[0] for r in self.ssh_records() if 'sh' in r['argv']]

    def test_auto_ssh_probe_once_and_verbose_selection(self):
        result = self.cli('--transport', 'auto', '-v', 'status')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Transport: ssh, host slop', result.stdout)
        self.assertEqual(result.stderr, 'slopctl: using ssh (host slop)\n')
        probes = [r for r in self.ssh_records() if r['argv'][-1] == 'true']
        self.assertEqual(len(probes), 1)
        self.assertEqual(probes[0]['argv'], ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=3', 'slop', 'true'])
        self.assertEqual(len(self.records()), 1)  # Only the setup call used adb.
        self.assertTrue(all(base64.b64decode(r['bytes']) == b'' for r in self.ssh_records()))

    def test_auto_adb_fallback_once_and_quiet_by_default(self):
        self.configure_ssh(probe_rc=255)
        result = self.cli('--transport', 'auto', 'status')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Transport: adb, serial SLOP', result.stdout)
        self.assertEqual(result.stderr, '')
        self.assertEqual(len(self.ssh_records()), 1)
        self.assertTrue(any('shell' in r['argv'] for r in self.records()))

    def test_nonzero_remote_exit_surfaces(self):
        self.configure_ssh(rules=[{'contains': 'bad', 'output': 'failed', 'rc': 7}])
        result = self.cli('--transport', 'ssh', 'shell', 'bad')
        self.assertEqual(result.returncode, 1)
        self.assertIn('device command exited 7: failed', result.stderr)
        self.assertNotIn('Traceback', result.stderr)

    def test_push_and_pull_binary_roundtrip_with_quoted_paths(self):
        self.configure_ssh(execute_shell=True)
        data = b'first\r\n\x00last\xff\r\n'
        source = self.root / 'source'
        remote = self.root / "remote ' ;$(false)"
        target = self.root / 'target'
        source.write_bytes(data)
        ssh = slop.Ssh()
        ssh.push(source, remote)
        self.assertEqual(remote.read_bytes(), data)
        self.assertEqual(base64.b64decode(self.ssh_records()[0]['bytes']), data)
        self.assertIn('cat > ', self.ssh_commands()[0])
        self.assertIn('&& mv -f ', self.ssh_commands()[0])
        self.assertFalse(list(self.root.glob('.slopctl-*')))
        ssh.pull(remote, target)
        self.assertEqual(target.read_bytes(), data)
        self.assertEqual(self.ssh_commands()[1], 'cat ' + slop.q(remote))
        self.assertEqual(self.ssh_records()[0]['argv'][:5], ['-o', 'BatchMode=yes', 'slop', 'sh', '-c'])

    def test_pull_canned_bytes_exactly(self):
        data = bytes(range(256)) + b'\r\n\0'
        self.configure_ssh(rules=[{'contains': 'cat ', 'bytes': base64.b64encode(data).decode()}])
        target = self.root / 'pull'
        slop.Ssh().pull('/tmp/payload', target)
        self.assertEqual(target.read_bytes(), data)

    def test_shell_preserves_crlf_and_real_exit_status(self):
        self.configure_ssh(execute_shell=True)
        ssh = slop.Ssh()
        self.assertEqual(ssh.shell("printf 'a\\r\\nb'"), 'a\r\nb')
        with self.assertRaisesRegex(slop.Error, 'exited 12'):
            ssh.shell('exit 12')

    def test_awake_command_shape(self):
        for state, command in [('on', ': > /run/slop-awake'), ('off', 'rm -f /run/slop-awake')]:
            result = self.cli('--transport', 'ssh', 'awake', state)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(command, self.ssh_commands())
        previous = len(self.ssh_commands())
        result = self.cli('--transport', 'ssh', 'awake', 'status')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(self.ssh_commands()[previous:],
                         ['if [ -e /run/slop-awake ]; then echo on; else echo off; fi'])

    def test_awake_flag_creation_removal_and_status(self):
        self.configure_ssh(execute_shell=True)
        flag = self.root / 'awake'
        with patch.object(slop, 'AWAKE', str(flag)), contextlib.redirect_stdout(io.StringIO()) as output:
            ssh = slop.Ssh()
            slop.awake(ssh, 'status')
            slop.awake(ssh, 'on')
            self.assertTrue(flag.is_file())
            slop.awake(ssh, 'status')
            slop.awake(ssh, 'off')
            self.assertFalse(flag.exists())
        self.assertEqual(output.getvalue(), 'Awake: off\nAwake: on\nAwake: on\nAwake: off\n')

    def test_forced_ssh_failure_does_not_fallback(self):
        self.configure_ssh(probe_rc=255)
        result = self.cli('--transport', 'ssh', 'status')
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(self.records()), 1)
        self.assertNotIn('Traceback', result.stderr)

    def test_auto_is_default_without_environment_override(self):
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop('SLOPCTL_TRANSPORT', None)
            result = self.cli('status')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Transport: ssh', result.stdout)
        self.assertEqual(result.stderr, '')

    def test_restart_sigkill_fallback(self):
        self.configure_ssh(rules=[{'contains': 'term_timeout', 'output': 'term_timeout'},
                                  {'contains': 'n=0\nwhile :', 'output': '200'},
                                  {'contains': 'slot_pids', 'output': '100'}])
        result = self.cli('--transport', 'ssh', 'restart')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, 'slot pid: 200\n')
        self.assertIn('SIGKILL, saves may be lost', result.stderr)
        commands = self.ssh_commands()
        term = next(i for i, c in enumerate(commands) if 'kill -TERM 100' in c)
        kill = next(i for i, c in enumerate(commands) if 'kill -KILL 100' in c)
        respawn = next(i for i, c in enumerate(commands) if 'n=0\nwhile :' in c)
        self.assertLess(term, kill)
        self.assertLess(kill, respawn)
        self.assertIn('"$n" -lt 10', commands[term + 1])
        self.assertIn('slot did not exit after SIGKILL', commands[kill + 1])
        self.assertIn("old=' 100 '", commands[respawn])

    def test_shot_streams_visible_page_without_temp(self):
        data = bytes.fromhex('0000ff00 00ff00ff ff000000 ffffffff')
        self.configure_ssh(rules=[{'contains': 'virtual_size', 'output':
                                  'virtual_size=2,4\nbits_per_pixel=32\nstride=8\npan=0,2\nmodes=U:2x2p-60\n'},
                                 {'contains': 'dd if=', 'bytes': base64.b64encode(data).decode()}])
        out = self.root / 'shot.png'
        result = self.cli('--transport', 'ssh', 'shot', str(out))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(out.read_bytes(), slop.encode_png(data, 2, 2, 32, 8))
        self.assertEqual(self.ssh_commands()[-1], 'dd if=/dev/fb0 bs=8 skip=2 count=2')
        self.assertFalse(any('of=' in c or 'slopctl-fb' in c for c in self.ssh_commands()))

    def test_environment_defaults_and_cli_overrides(self):
        with patch.dict(os.environ, {'SLOPCTL_TRANSPORT': 'ssh', 'SLOPCTL_HOST': 'alternate',
                                     'SLOPCTL_SRC': '/configured/dist'}):
            args = slop.parser().parse_args(['deploy'])
            self.assertEqual((args.transport, args.host, args.src), ('ssh', 'alternate', '/configured/dist'))
            result = self.cli('status')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('host alternate', result.stdout)
            result = self.cli('--transport', 'adb', 'status')
            self.assertEqual(result.returncode, 0)
            self.assertIn('Transport: adb', result.stdout)

    def test_source_missing_requires_explicit_path(self):
        with patch.dict(os.environ, {'SLOPCTL_SRC': ''}), patch.object(Path, 'is_dir', return_value=False):
            self.assertIsNone(slop.default_source())
        with self.assertRaisesRegex(slop.Error, 'Supply --src'):
            slop.deploy(slop.Ssh(), argparse.Namespace(src=None))

    def test_repo_relative_source_and_explicit_override(self):
        expected = PATH.resolve().parents[2].parent / 'slop-device-build/dist-device'
        with patch.object(Path, 'is_dir', return_value=True):
            self.assertEqual(slop.default_source(), str(expected))
        with patch.dict(os.environ, {'SLOPCTL_SRC': '/configured/dist'}):
            self.assertEqual(slop.parser().parse_args(['deploy', '--src', '/explicit/dist']).src, '/explicit/dist')

    def test_ssh_timeouts_remain_bounded(self):
        with patch.object(slop.subprocess, 'run', side_effect=subprocess.TimeoutExpired('ssh', 12)) as run:
            with self.assertRaisesRegex(slop.Error, 'timed out after 12 s'):
                slop.Ssh().shell('true', timeout=12)
            self.assertEqual(run.call_args.kwargs['timeout'], 12)
            self.assertEqual(run.call_args.kwargs['stdin'], subprocess.DEVNULL)


class EncodingTests(unittest.TestCase):
    def png_chunks(self, png):
        self.assertEqual(png[:8], b'\x89PNG\r\n\x1a\n')
        chunks = {}
        pos = 8
        while pos < len(png):
            length = struct.unpack_from('>I', png, pos)[0]
            kind = png[pos + 4:pos + 8]
            data = png[pos + 8:pos + 8 + length]
            self.assertEqual(struct.unpack_from('>I', png, pos + 8 + length)[0], zlib.crc32(kind + data))
            chunks[kind] = data
            pos += length + 12
        return chunks

    def test_png_bgra_two_by_two_roundtrip(self):
        raw = bytes.fromhex('0000ff00 00ff00ff ff000000 ffffffff')
        chunks = self.png_chunks(slop.encode_png(raw, 2, 2, 32, 8))
        self.assertEqual(struct.unpack('>IIBBBBB', chunks[b'IHDR']), (2, 2, 8, 2, 0, 0, 0))
        self.assertEqual(zlib.decompress(chunks[b'IDAT']), b'\0\xff\0\0\0\xff\0\0\0\0\xff\xff\xff\xff')

    def test_png_rgb565_padding_and_rotation(self):
        raw = struct.pack('<HH', 0xf800, 0x07e0) + bytes(4)
        chunks = self.png_chunks(slop.encode_png(raw, 2, 1, 16, 8, 90))
        self.assertEqual(struct.unpack_from('>II', chunks[b'IHDR']), (1, 2))
        self.assertEqual(zlib.decompress(chunks[b'IDAT']), b'\0\xff\0\0\0\0\xff\0')
        with self.assertRaises(slop.Error):
            slop.encode_png(raw[:-1], 2, 1, 16, 8)

    def test_hat_press_and_release(self):
        self.assertEqual(struct.unpack('<qqHHi', slop.event_bytes('LEFT')[:24]), (0, 0, 3, 16, -1))
        self.assertEqual(struct.unpack('<qqHHi', slop.event_bytes('LEFT', False)[:24]), (0, 0, 3, 16, 0))

    def test_sequence_validation(self):
        self.assertEqual(slop.parse_seq('A 100, wait 500, DOWN, START'),
                         [('A', 100), ('WAIT', 500), ('DOWN', 100), ('START', 100)])
        for invalid in ('A -1', 'WAIT', 'unknown', 'WAIT 10001', 'WAIT 10000, WAIT 10000, A', ''):
            with self.assertRaises((slop.Error, ValueError)):
                slop.parse_seq(invalid)

    def test_missing_sentinel_is_error(self):
        with self.assertRaises(slop.Error):
            slop.parse_shell('some output\r\n')


if __name__ == '__main__':
    unittest.main()
