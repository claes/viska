{ pkgs, self }:
pkgs.testers.runNixOSTest {
  name = "viska-cross-uid";
  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];
    services.viska = {
      enable = true; backend = "mock"; keyboardLayout = "se"; ttlSeconds = 45;
      authorization = {
        sendUsers = [ "producer" ]; typeUsers = [ "trigger" "reader2" ]; outputUsers = [ "reader" "reader2" ];
        clearUsers = [ "trigger" ]; statusUsers = [ "producer" "trigger" ];
      };
    };
    users.users = {
      producer.isNormalUser = true;
      reader.isNormalUser = true;
      reader2.isNormalUser = true;
      trigger.isNormalUser = true;
      # Deliberately grant socket access to test the daemon's credential checks.
      outsider = { isNormalUser = true; extraGroups = [ "viska-clients" ]; };
      blocked.isNormalUser = true;
    };
    environment.systemPackages = [ pkgs.python3 pkgs.acl ];
    environment.etc."viska/integration.py".source = ../tests/integration.py;
    environment.etc."viska/change-reader.py".text = ''
      from pathlib import Path
      path = Path("/etc/passwd")
      rows = [line.split(":") for line in path.read_text().splitlines()]
      for row in rows:
          if row[0] == "reader":
              row[2] = "23457"
      path.write_text("\n".join(":".join(row) for row in rows) + "\n")
    '';
  };
  testScript = ''
    start_all()
    machine.wait_for_unit("viskad.service")
    machine.succeed("test $(stat -c %a /run/viska) = 750")
    machine.succeed("test $(stat -c %a /run/viska/submit.sock) = 660")
    machine.succeed("su - producer -c \"printf '%s' 'räksmörgås ÅÄÖ @ €' | viskactl send\"")
    machine.succeed("su - trigger -c 'viskactl status' | grep pending")
    for command in ["type", "clear", "status", "send"]:
        machine.fail("su - outsider -c 'viskactl " + command + " </dev/null'")
    machine.fail("su - blocked -c 'viskactl status'")
    machine.fail("su - producer -c 'viskactl type'")
    machine.fail("su - trigger -c \"printf x | viskactl send\"")
    machine.succeed("su - trigger -c 'viskactl type'")
    machine.fail("su - trigger -c 'viskactl type'")
    machine.succeed("su - producer -c 'printf x | viskactl send'")
    machine.succeed("systemctl restart viskad.service")
    machine.succeed("su - trigger -c 'viskactl status' | grep empty")
    # Change the authorized user's UID without restarting the service: lookup
    # must use current account data rather than a startup identity cache.
    machine.succeed("loginctl terminate-user trigger || true; usermod -u 23456 trigger")
    machine.succeed("su - producer -c 'printf x | viskactl send'")
    machine.succeed("su - trigger -c 'viskactl type'")
    machine.fail("su - trigger -c 'viskactl output'")
    machine.succeed("(set +e; su - reader -c 'viskactl output --channel binary --sync'; echo $? > /tmp/viska-output.rc) > /tmp/viska-output 2>/tmp/viska-output.err < /dev/null &")
    machine.sleep(1)
    machine.succeed("su - producer -c \"printf 'line\\n' | viskactl send --channel binary\"")
    machine.wait_until_succeeds("test -s /tmp/viska-output")
    machine.wait_until_succeeds("test -s /tmp/viska-output.rc")
    machine.succeed("test $(cat /tmp/viska-output.rc) = 0")
    machine.succeed("test $(wc -c < /tmp/viska-output) = 5")
    machine.succeed("su - producer -c 'printf restricted | viskactl send --recipient reader'")
    machine.fail("su - reader2 -c 'viskactl output'")
    machine.fail("su - trigger -c 'viskactl type'")
    machine.succeed("su - reader -c 'viskactl output' | grep restricted")
    machine.succeed("su - producer -c 'printf x | viskactl send --recipient trigger'")
    machine.fail("su - reader -c 'viskactl output'")
    machine.fail("su - reader2 -c 'viskactl type'")
    machine.succeed("su - trigger -c 'viskactl type'")
    machine.succeed("(set +e; su - reader2 -c 'viskactl output --channel restricted --sync'; echo $? > /tmp/restricted.rc) > /tmp/restricted.out 2>/tmp/restricted.err < /dev/null &")
    machine.wait_until_succeeds("su - trigger -c 'viskactl status --channel restricted' | grep waiting")
    machine.succeed("su - producer -c 'printf kept | viskactl send --channel restricted --recipient reader'")
    machine.wait_until_succeeds("test -s /tmp/restricted.rc")
    machine.succeed("test $(cat /tmp/restricted.rc) != 0; test ! -s /tmp/restricted.out")
    machine.succeed("su - reader -c 'viskactl output --channel restricted' | grep kept")
    # Existing connection has the old UID: a fresh lookup before delivery must
    # reject it and preserve the newly stored value for the new account identity.
    machine.succeed("(set +e; su - reader -c 'viskactl output --channel identity --sync'; echo $? > /tmp/identity.rc) > /tmp/identity.out 2>/tmp/identity.err < /dev/null &")
    machine.wait_until_succeeds("su - trigger -c 'viskactl status --channel identity' | grep waiting")
    machine.succeed("python3 /etc/viska/change-reader.py; systemctl try-restart nscd.service")
    machine.succeed("su - producer -c 'printf kept | viskactl send --channel identity --recipient reader'")
    machine.wait_until_succeeds("test -s /tmp/identity.rc")
    machine.succeed("test $(cat /tmp/identity.rc) != 0; test ! -s /tmp/identity.out")
    machine.succeed("su - reader -c 'viskactl output --channel identity' | grep kept")
    machine.succeed("systemctl stop viskad.service; test ! -e /run/viska/control.sock")
    # Run the same real-socket/mock suite as the host test app, under a
    # non-root producer account with actual root-owned filesystem ancestors.
    machine.succeed("su - producer -c 'VISKA_BIN_DIR=${self.packages.x86_64-linux.default}/bin python3 /etc/viska/integration.py'", timeout=120)
  '';
}
