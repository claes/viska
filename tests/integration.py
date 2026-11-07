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
import struct
import subprocess
import tempfile
import time
import unittest

BIN = Path(os.environ.get('VISKA_BIN_DIR', 'target/debug')).resolve()
USER = pwd.getpwuid(os.getuid()).pw_name
MAGIC = b'VISKA\0\x01\0'

class Service(unittest.TestCase):
    def setUp(self):
        # Nix's /build (TMPDIR) may be writable by unrelated build identities;
        # use a private child of the sticky /tmp instead, as the real demo does.
        self.temp = tempfile.TemporaryDirectory(prefix='viska-test-', dir='/tmp')
        self.root = Path(self.temp.name)
        self.config = dict(backend='mock', keyboard_layout='se', ttl_seconds=45,
            submission_socket=str(self.root / 'submit.sock'), control_socket=str(self.root / 'control.sock'),
            authorization={k:[USER] for k in ['store_users','type_users','clear_users','status_users']})
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
    def ctl(self, command, value=None):
        return subprocess.run([str(BIN/'viskactl'), command], input=value, env=self.env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=12)
    def status(self):
        result = self.ctl('status'); self.assertEqual(result.returncode, 0, result.stderr)
        value = json.loads(result.stdout)
        self.assertEqual(set(value), {'state','ttl_remaining_ms','backend_ready'})
        return value['state']
    def store(self, value):
        result = self.ctl('store', value); self.assertEqual(result.returncode, 0, result.stderr); self.assertEqual(result.stdout, b'')
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
    def header(self, n): return MAGIC + struct.pack('<Q', n)
    def test_persistence_swedish_and_one_shot(self):
        self.store('räksmörgås ÅÄÖ @ €'.encode())
        self.assertEqual(self.status(), 'pending')
        self.assertEqual(self.ctl('type').returncode, 0)
        self.assertEqual(self.ctl('type').returncode, 1)
        self.assertEqual(self.status(), 'empty')
    def test_invalid_preserves_existing(self):
        self.store(b'public-fixture')
        for value in [b'line\n', b'\xff', b'\0', '😀'.encode(), b'^']:
            self.assertNotEqual(self.ctl('store', value).returncode, 0)
            self.assertEqual(self.status(), 'pending')
    def test_limits_and_chunked_utf8(self):
        self.store(b'a' * 1048576)
        self.assertNotEqual(self.ctl('store', b'a'*1048577).returncode, 0)
        self.assertEqual(self.status(), 'pending')
        self.assertEqual(self.ctl('clear').returncode, 0)
        # A code point split across transport packets must still validate.
        self.assertTrue(self.raw([self.header(2), b'\xc3', b'\xa5', b'COMMIT1'])['ok'])
        self.assertEqual(self.status(), 'pending')
    def test_malformed_and_truncated_packets_preserve(self):
        self.store(b'old')
        cases = [[b'bad'], [self.header(1048577)], [self.header(3), b'a'],
                 [self.header(1), b'ab'], [self.header(1), b'a', b'bad'],
                 [self.header(1), b'a', b'COMMIT1', b'extra'], [b'x'*20000],
                 [self.header(1), b'a', b'COMMIT1', b'', b'extra'],
                 [self.header(1), b'a', b'COMMIT1', b'']]
        for packets in cases:
            self.assertFalse(self.raw(packets)['ok'], packets[0][:16])
            self.assertEqual(self.status(), 'pending')
    def test_expiry_and_clear(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['ttl_seconds'] = 1; self.start()
        self.store(b'x'); time.sleep(1.1)
        self.assertEqual(self.status(), 'empty'); self.assertNotEqual(self.ctl('type').returncode, 0)
        self.store(b'x'); self.assertEqual(self.ctl('clear').returncode, 0); self.assertEqual(self.status(), 'empty')
    def test_concurrent_triggers(self):
        self.store(b'abc')
        with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
            codes = list(pool.map(lambda _: self.ctl('type').returncode, range(12)))
        self.assertEqual(codes.count(0), 1)
        self.assertEqual(self.status(), 'empty')
    def test_concurrent_producers(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            codes = list(pool.map(lambda n: self.ctl('store', bytes([97+n])*65536).returncode, range(4)))
        self.assertIn(0, codes); self.assertEqual(self.status(), 'pending')
    def test_cancel_and_disconnect_do_not_restore(self):
        for disconnect in [False, True]:
            self.store(b'a'*1000)
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
        self.store(b'x')
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as s:
            s.settimeout(8); s.connect(str(self.root/'submit.sock')); s.send(self.header(3))
            time.sleep(1.1); self.assertEqual(self.status(), 'empty')
            self.assertFalse(json.loads(s.recv(2048))['ok'])
    def test_unauthorized_operation_even_with_endpoint_access(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['authorization'] = {k:[] for k in self.config['authorization']}
        self.start()
        for cmd in ['store','type','clear','status']:
            result = self.ctl(cmd, b'x' if cmd == 'store' else None)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, b'')
    def test_unknown_account_fails_closed(self):
        self.daemon.terminate(); self.daemon.communicate(timeout=5)
        self.config['authorization']['type_users'] = [USER, 'viska-missing-test-account']
        self.start(); self.store(b'x')
        self.assertNotEqual(self.ctl('type').returncode, 0)
        self.assertEqual(self.status(), 'pending')
    def test_descriptor_passing_is_rejected(self):
        self.store(b'old')
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as s, open('/dev/null', 'rb') as fd:
            s.settimeout(8); s.connect(str(self.root/'submit.sock'))
            s.sendmsg([self.header(1)], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd.fileno()]))])
            self.assertFalse(json.loads(s.recv(2048))['ok'])
        self.assertEqual(self.status(), 'pending')
    def test_memory_lock_failure_is_closed(self):
        def no_lock(): resource.setrlimit(resource.RLIMIT_MEMLOCK, (0, 0))
        result = subprocess.run([str(BIN/'viskactl'), 'store'], input=b'public-fixture', env=self.env,
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

if __name__ == '__main__': unittest.main(verbosity=2)
