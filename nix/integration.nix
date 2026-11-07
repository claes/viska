{ pkgs, self }:
pkgs.testers.runNixOSTest {
  name = "viska-cross-uid";
  nodes.machine = { ... }: {
    imports = [ self.nixosModules.default ];
    services.viska = {
      enable = true; backend = "mock"; keyboardLayout = "se"; ttlSeconds = 45;
      authorization = {
        storeUsers = [ "producer" ]; typeUsers = [ "trigger" ];
        clearUsers = [ "trigger" ]; statusUsers = [ "producer" "trigger" ];
      };
    };
    users.users = {
      producer.isNormalUser = true;
      trigger.isNormalUser = true;
      # Deliberately grant socket access to test the daemon's credential checks.
      outsider = { isNormalUser = true; extraGroups = [ "viska-clients" ]; };
      blocked.isNormalUser = true;
    };
    environment.systemPackages = [ pkgs.python3 pkgs.acl ];
    environment.etc."viska/integration.py".source = ../tests/integration.py;
  };
  testScript = ''
    start_all()
    machine.wait_for_unit("viskad.service")
    machine.succeed("test $(stat -c %a /run/viska) = 750")
    machine.succeed("test $(stat -c %a /run/viska/submit.sock) = 660")
    machine.succeed("su - producer -c \"printf '%s' 'räksmörgås ÅÄÖ @ €' | viskactl store\"")
    machine.succeed("su - trigger -c 'viskactl status' | grep pending")
    for command in ["type", "clear", "status", "store"]:
        machine.fail("su - outsider -c 'viskactl " + command + " </dev/null'")
    machine.fail("su - blocked -c 'viskactl status'")
    machine.fail("su - producer -c 'viskactl type'")
    machine.fail("su - trigger -c \"printf x | viskactl store\"")
    machine.succeed("su - trigger -c 'viskactl type'")
    machine.fail("su - trigger -c 'viskactl type'")
    machine.succeed("su - producer -c 'printf x | viskactl store'")
    machine.succeed("systemctl restart viskad.service")
    machine.succeed("su - trigger -c 'viskactl status' | grep empty")
    # Change the authorized user's UID without restarting the service: lookup
    # must use current account data rather than a startup identity cache.
    machine.succeed("loginctl terminate-user trigger || true; usermod -u 23456 trigger")
    machine.succeed("su - producer -c 'printf x | viskactl store'")
    machine.succeed("su - trigger -c 'viskactl type'")
    machine.succeed("systemctl stop viskad.service; test ! -e /run/viska/control.sock")
    # Run the same real-socket/mock suite as the host test app, under a
    # non-root producer account with actual root-owned filesystem ancestors.
    machine.succeed("su - producer -c 'VISKA_BIN_DIR=${self.packages.x86_64-linux.default}/bin python3 /etc/viska/integration.py'", timeout=120)
  '';
}
