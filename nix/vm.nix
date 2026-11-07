{ pkgs, modulesPath, ... }:
{
  imports = [ (modulesPath + "/virtualisation/qemu-vm.nix") ];
  networking.hostName = "viska-test-vm";
  system.stateVersion = "25.11";
  virtualisation = {
    memorySize = 2048;
    cores = 2;
    graphics = true;
    # QEMU's fallback is slower, but permits interactive testing without KVM.
    qemu.options = [ "-machine" "accel=kvm:tcg" ];
  };
  users.users = {
    root.initialPassword = "root";
    producer = { isNormalUser = true; initialPassword = "producer"; };
    trigger = { isNormalUser = true; initialPassword = "trigger"; };
    outsider = { isNormalUser = true; initialPassword = "outsider"; };
  };
  services.viska = {
    enable = true;
    keyboardLayout = "se";
    authorization = {
      storeUsers = [ "producer" ]; typeUsers = [ "trigger" ];
      clearUsers = [ "trigger" ]; statusUsers = [ "producer" "trigger" ];
    };
  };
  services.xserver = {
    enable = true;
    xkb.layout = "se";
    desktopManager.xfce.enable = true;
    displayManager.lightdm.enable = true;
  };
  services.displayManager.autoLogin = { enable = true; user = "trigger"; };
  environment.systemPackages = [ pkgs.mousepad pkgs.xterm ];
  environment.etc."viska/VM-README.txt".text = ''
    Disposable Swedish-layout Viska test VM (nonsensitive values only).
    Open Mousepad and two terminals. In the producer terminal run:
      su - producer
      printf '%s' 'räksmörgås ÅÄÖ @ €' | viskactl store
    In a trigger terminal run:
      sleep 3; viskactl type
    Focus Mousepad during the three seconds. It should receive the text once.
    Subsequent typing fails. Test clear, expiry and unsupported characters too.
    root/producer/trigger/outsider passwords equal their usernames (VM only).
    Inspect the service as root with journalctl -u viskad.
    Shutdown using the desktop menu. Host home and clipboard are not shared.
  '';
}
