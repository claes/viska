#!/usr/bin/env python3
"""Unprivileged integration tests. Every value below is a public test fixture."""
import concurrent.futures
import array
import json
import os
from pathlib import Path
import pwd
import resource
import socket
import subprocess
import tempfile
import time
import unittest

BIN = Path(os.environ.get('VISKA_BIN_DIR', 'target/debug')).resolve()
USER = pwd.getpwuid(os.getuid()).pw_name

class Service(unittest.TestCase):
    def setUp(self):
        # Nix's /build (TMPDIR) may be writable by unrelated build identities;
        # use a private child of the sticky /tmp instead, as the real demo does.
        self.temp = tempfile.TemporaryDirectory(prefix='viska-test-', dir='/tmp')
        self.root = Path(self.temp.name)
        self.config = dict(backend='mock', keyboard_layout='se', ttl_seconds=45, max_channels=2,
            submission_socket=str(self.root / 'submit.sock'), control_socket=str(self.root / 'control.sock'),
            authorization={k:[USER] for k in ['send_users','type_users','output_users','clear_users','status_users']})
        self.start()
    def start(self):
        path = self.root / 'config.json'; path.write_text(json.dumps(self.config))
        self.daemon = subprocess.Popen([str(BIN/'viskad'), '--config', str(path)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.env = dict(os.environ, VISKA_SOCKET_DIR=str(self.root))
        for _ in range(100):
            if self.daemon.poll() is not None:
                details = [(str(p), p.lstat().st_uid, oct(p.lstat().st_mode & 0o7777)) for p in self.root.parents]
                self.fail('daemon failed: ' + self.daemon.communicate()[1].decode() + repr(details))
            if (self.root/'control.sock').exists():
                status = self.ctl('status')
                if status.returncode == 0 or b'not authorized' in status.stderr: return
            time.sleep(.02)
        self.fail('daemon startup timeout')
    def tearDown(self):
        if self.daemon.poll() is None: self.daemon.terminate()
        out, err = self.daemon.communicate(timeout=5)
        self.assertEqual(out, b'')
        self.assertNotIn(b'public-fixture', err)
        self.assertFalse((self.root/'submit.sock').exists())
        self.temp.cleanup()
    def ctl(self, command, value=None, channel="default", sync=False, recipient=None):
        return subprocess.run([str(BIN/'viskactl'), command, '--channel', channel] + (['--sync'] if sync else []) + (['--recipient', recipient] if recipient is not None else []), input=value, env=self.env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=12)
    def status(self):
        result = self.ctl('status'); self.assertEqual(result.returncode, 0, result.stderr)
        value = json.loads(result.stdout)
        self.assertEqual(set(value), {'state','ttl_remaining_ms','backend_ready'})
        return value['state']
    def send(self, value):
        result = self.ctl('send', value); self.assertEqual(result.returncode, 0, result.stderr); self.assertEqual(result.stdout, b'')
    def raw(self, packets):
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as s:
            s.settimeout(8); s.connect(str(self.root/'submit.sock'))
            for packet in packets:
                try: s.sendall(packet)
                except (BrokenPipeError, ConnectionResetError): break
            try: s.shutdown(socket.SHUT_WR)
            except OSError: pass
            # Closing a rejected connection with unread packets can reset the
            # connection before Linux delivers its metadata error response.
            try: reply = s.recv(2048)
            except ConnectionResetError: return {'ok': False}
            return json.loads(reply) if reply else {'ok': False}
    def header(self, n): return json.dumps(dict(version=3, operation='send', length=n)).encode()
    def test_persistence_swedish_and_one_shot(self):
        self.send('räksmörgås ÅÄÖ @ €'.encode())
        self.assertEqual(self.status(), 'pending')
        self.assertEqual(self.ctl('type').returncode, 0)
        self.assertEqual(self.ctl('type').returncode, 1)
        self.assertEqual(self.status(), 'empty')
    def test_binary_output_and_type_validation(self):
        for value in [b'line\n', b'\xff', b'\0', '😀'.encode(), b'^']:
            self.send(value)
            self.assertNotEqual(self.ctl('type').returncode, 0)
            self.assertEqual(self.status(), 'pending')
            result = self.ctl('output'); self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, value)
        self.send(b''); self.assertEqual(self.ctl('output').stdout, b'')
    def test_limits_and_chunked_utf8(self):
        self.send(b'a' * 1048576)
        self.assertNotEqual(self.ctl('send', b'a'*1048577).returncode, 0)
        self.assertEqual(self.status(), 'pending')
        self.assertEqual(self.ctl('clear').returncode, 0)
        # A code point split across transport packets must still validate.
        self.assertTrue(self.raw([self.header(2), b'\xc3', b'\xa5', b'COMMIT3'])['ok'])
        self.assertEqual(self.status(), 'pending')
    def test_malformed_and_truncated_packets_preserve(self):
        self.send(b'old')
        cases = [[b'bad'], [self.header(1048577)], [self.header(3), b'a'],
                 [self.header(1), b'ab'], [self.header(1), b'a', b'bad'],
                 [self.header(1), b'a', b'COMMIT3', b'extra'], [b'x'*20000],
                 [self.header(1), b'a', b'COMMIT3', b'', b'extra'],
                 [self.header(1), b'a', b'COMMIT3', b'']]
        for packets in cases:
            self.assertFalse(self.raw(packets)['ok'], packets[0][:16])
            self.assertEqual(self.status(), 'pending')
    def test_expiry_and_clear(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['ttl_seconds'] = 1; self.start()
        self.send(b'x'); time.sleep(1.1)
        self.assertEqual(self.status(), 'empty'); self.assertNotEqual(self.ctl('type').returncode, 0)
        self.send(b'x'); self.assertEqual(self.ctl('clear').returncode, 0); self.assertEqual(self.status(), 'empty')
    def test_concurrent_triggers(self):
        self.send(b'abc')
        with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
            codes = list(pool.map(lambda _: self.ctl('type').returncode, range(12)))
        self.assertEqual(codes.count(0), 1)
        self.assertEqual(self.status(), 'empty')
    def test_concurrent_producers(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            codes = list(pool.map(lambda n: self.ctl('send', bytes([97+n])*65536).returncode, range(4)))
        self.assertIn(0, codes); self.assertEqual(self.status(), 'pending')
    def test_cancel_and_disconnect_do_not_restore(self):
        for disconnect in [False, True]:
            self.send(b'a'*1000)
            trigger = subprocess.Popen([str(BIN/'viskactl'), 'type'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            for _ in range(100):
                if self.status() == 'injecting': break
                time.sleep(.01)
            else: self.fail('injection did not start')
            if disconnect: trigger.terminate()
            else: self.assertEqual(self.ctl('clear').returncode, 0)
            trigger.communicate(timeout=3)
            self.assertNotEqual(trigger.returncode, 0)
            for _ in range(50):
                if self.status() == 'empty': break
                time.sleep(.01)
            self.assertEqual(self.status(), 'empty')
    def test_stalled_submission_does_not_block_expiry(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['ttl_seconds'] = 1; self.start()
        self.send(b'x')
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as s:
            s.settimeout(8); s.connect(str(self.root/'submit.sock')); s.send(self.header(3))
            time.sleep(1.1); self.assertEqual(self.status(), 'empty')
            self.assertFalse(json.loads(s.recv(2048))['ok'])
    def test_unauthorized_operation_even_with_endpoint_access(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['authorization'] = {k:[] for k in self.config['authorization']}
        self.start()
        for cmd in ['send','type','output','clear','status']:
            result = self.ctl(cmd, b'x' if cmd == 'send' else None)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, b'')
    def test_unknown_account_fails_closed(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['authorization']['type_users'] = [USER, 'viska-missing-test-account']
        self.start(); self.send(b'x')
        self.assertNotEqual(self.ctl('type').returncode, 0)
        self.assertEqual(self.status(), 'pending')
    def test_descriptor_passing_is_rejected(self):
        self.send(b'old')
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as s, open('/dev/null', 'rb') as fd:
            s.settimeout(8); s.connect(str(self.root/'submit.sock'))
            s.sendmsg([self.header(1)], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd.fileno()]))])
            self.assertFalse(json.loads(s.recv(2048))['ok'])
        self.assertEqual(self.status(), 'pending')
    def test_memory_lock_failure_is_closed(self):
        def no_lock(): resource.setrlimit(resource.RLIMIT_MEMLOCK, (0, 0))
        result = subprocess.run([str(BIN/'viskactl'), 'send'], input=b'public-fixture', env=self.env,
                                capture_output=True, preexec_fn=no_lock, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(b'public-fixture', result.stderr)
        self.assertEqual(self.status(), 'empty')
    def test_unsafe_endpoints_fail_closed(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        config_path = self.root/'config.json'
        def rejected():
            result = subprocess.run([str(BIN/'viskad'), '--config', str(config_path)],
                                    capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
        (self.root/'submit.sock').write_text('public fixture')
        rejected()
        self.assertEqual((self.root/'submit.sock').read_text(), 'public fixture')
        (self.root/'submit.sock').unlink()
        (self.root/'submit.sock').symlink_to(config_path)
        rejected(); self.assertTrue((self.root/'submit.sock').is_symlink())
        (self.root/'submit.sock').unlink()
        self.root.chmod(0o777)
        try: rejected()
        finally: self.root.chmod(0o700)

    def test_channels_waiters_and_clear(self):
        def waiting():
            return subprocess.Popen([str(BIN/'viskactl'), 'output', '--sync', '--channel', 'work'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        reader = waiting()
        try:
            for _ in range(100):
                if json.loads(self.ctl('status', channel='work').stdout)['state'] == 'waiting': break
                time.sleep(.02)
            else: self.fail('consumer did not wait')
            self.assertNotEqual(self.ctl('output', channel='work', sync=True).returncode, 0)
            self.send(b'default-value')
            self.assertEqual(self.ctl('send', b'work\n\0', channel='work').returncode, 0)
            out, err = reader.communicate(timeout=5)
            self.assertEqual(reader.returncode, 0, err); self.assertEqual(out, b'work\n\0')
            self.assertEqual(self.ctl('output').stdout, b'default-value')
        finally:
            if reader.poll() is None: reader.terminate(); reader.communicate(timeout=5)
        reader = waiting()
        try:
            self.send(b'keep-default')
            time.sleep(.2); self.assertEqual(self.ctl('clear', channel='work').returncode, 0)
            reader.communicate(timeout=5); self.assertNotEqual(reader.returncode, 0)
            self.assertEqual(self.ctl('output').stdout, b'keep-default')
        finally:
            if reader.poll() is None: reader.terminate(); reader.communicate(timeout=5)
    def test_disconnected_waiter_and_channel_limit(self):
        reader = subprocess.Popen([str(BIN/'viskactl'), 'output', '--sync'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        time.sleep(.2); reader.terminate(); reader.communicate(timeout=5)
        self.send(b'kept'); self.assertEqual(self.ctl('output').stdout, b'kept')
        for channel in ['a','b']: self.assertEqual(self.ctl('send', channel.encode(), channel=channel).returncode, 0)
        self.assertNotEqual(self.ctl('send', b'c', channel='c').returncode, 0)
        for channel in ['a','b']: self.assertEqual(self.ctl('output', channel=channel).stdout, channel.encode())
    def test_output_authorization_is_separate(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['authorization']['output_users'] = []
        self.start(); self.send(b'x')
        self.assertNotEqual(self.ctl('output').returncode, 0)
        self.assertEqual(self.status(), 'pending'); self.assertEqual(self.ctl('type').returncode, 0)

    def test_sync_type_beyond_old_timeouts(self):
        reader = subprocess.Popen([str(BIN/'viskactl'), 'type', '--sync'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            time.sleep(36)
            self.assertIsNone(reader.poll())
            self.send(b'hello')
            out, err = reader.communicate(timeout=5)
            self.assertEqual(reader.returncode, 0, err); self.assertEqual(out, b'')
        finally:
            if reader.poll() is None: reader.terminate(); reader.communicate(timeout=5)
    def test_max_output_broken_pipe_and_clear(self):
        value = bytes(range(256)) * 4096
        self.send(value)
        result = self.ctl('output'); self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, value)
        for clear in [False, True]:
            self.send(value)
            reader = subprocess.Popen([str(BIN/'viskactl'), 'output'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                time.sleep(.2)
                if clear: self.assertEqual(self.ctl('clear').returncode, 0)
                else: reader.stdout.close()
                reader.wait(timeout=5)
                self.assertNotEqual(reader.returncode, 0)
                self.assertEqual(self.status(), 'empty')
            finally:
                if reader.poll() is None: reader.terminate(); reader.wait(timeout=5)
                if reader.stdout: reader.stdout.close()
                reader.stderr.close()
    def test_parallel_channels(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda name: self.ctl('send', name.encode()*32768, channel=name), ['a','b']))
        for result in results: self.assertEqual(result.returncode, 0, result.stderr)
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda name: self.ctl('output', channel=name), ['a','b']))
        for name, result in zip(['a','b'], results):
            self.assertEqual(result.returncode, 0, result.stderr); self.assertEqual(result.stdout, name.encode()*32768)

    def test_recipient_validation_and_replacement(self):
        self.send(b'old')
        for name in ['', 'unlisted-user', 'bad\x00name']:
            # NUL cannot be represented in argv; test it through raw IPC instead.
            if '\0' in name:
                header=json.dumps(dict(version=3,operation='send',length=1,recipient=name)).encode()
                self.assertFalse(self.raw([header,b'x',b'COMMIT3'])['ok'])
            else: self.assertNotEqual(self.ctl('send',b'x',recipient=name).returncode,0)
            self.assertEqual(self.status(),'pending')
        self.assertEqual(self.ctl('output').stdout,b'old')
        result=self.ctl('send',b'restricted',recipient=USER)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(self.ctl('output').stdout,b'restricted')
        self.assertEqual(self.ctl('send',b'x',recipient=USER).returncode,0)
        self.assertEqual(self.ctl('type').returncode,0)
        self.assertEqual(self.ctl('send',b'first',recipient=USER).returncode,0)
        self.send(b'unrestricted'); self.assertEqual(self.ctl('output').stdout,b'unrestricted')
    def test_missing_listed_recipient_and_operation_permissions(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['authorization']['output_users'] = ['viska-nonexistent-recipient']
        self.start(); self.send(b'old')
        self.assertNotEqual(self.ctl('send',b'x',recipient='viska-nonexistent-recipient').returncode,0)
        self.assertEqual(self.status(),'pending')
        self.assertEqual(self.ctl('send',b'x',recipient=USER).returncode,0) # type-only recipient
        self.assertNotEqual(self.ctl('output').returncode,0)
        self.assertEqual(self.ctl('type').returncode,0)
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['authorization']['type_users'] = []
        self.config['authorization']['output_users'] = [USER]
        self.start()
        self.assertEqual(self.ctl('send',b'raw\n',recipient=USER).returncode,0)
        self.assertNotEqual(self.ctl('type').returncode,0)
        self.assertEqual(self.ctl('output').stdout,b'raw\n')
    def test_help_and_old_interfaces(self):
        for cmd in [[],['send'],['type'],['output'],['clear'],['status']]:
            result=subprocess.run([str(BIN/'viskactl'),*cmd,'--help'],capture_output=True,env=dict(self.env,VISKA_SOCKET_DIR='/nonexistent'),timeout=5)
            self.assertEqual(result.returncode,0); self.assertIn(b'Usage:',result.stdout); self.assertIn(b'General options:',result.stdout)
            if cmd==['send']: self.assertIn(b'--recipient USER',result.stdout);self.assertNotIn(b'--sync',result.stdout)
        self.assertNotEqual(self.ctl('store',b'x').returncode,0)
        self.assertNotEqual(self.ctl('output',recipient=USER).returncode,0)
        self.assertFalse(self.raw([json.dumps(dict(version=2,operation='send',length=0)).encode()])['ok'])

if __name__ == '__main__': unittest.main(verbosity=2)
