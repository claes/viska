{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.viska;
  format = pkgs.formats.json {};
  settings = {
    keyboard_layout = cfg.keyboardLayout;
    ttl_seconds = cfg.ttlSeconds;
    max_payload_bytes = cfg.maxPayloadBytes;
    backend = cfg.backend;
    authorization = {
      store_users = cfg.authorization.storeUsers;
      type_users = cfg.authorization.typeUsers;
      clear_users = cfg.authorization.clearUsers;
      status_users = cfg.authorization.statusUsers;
    };
  };
  configFile = format.generate "viska-config.json" settings;
  clientUsers = lib.unique (cfg.authorization.storeUsers ++ cfg.authorization.typeUsers
    ++ cfg.authorization.clearUsers ++ cfg.authorization.statusUsers);
  authOption = description: lib.mkOption {
    type = lib.types.listOf lib.types.str;
    default = [];
    inherit description;
  };
in {
  options.services.viska = {
    enable = lib.mkEnableOption "Viska one-shot text delivery";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "Package containing viskad and viskactl.";
    };
    keyboardLayout = lib.mkOption {
      type = lib.types.enum [ "us" "se" ]; default = "us";
      description = "Must match the target session's active keyboard layout.";
    };
    ttlSeconds = lib.mkOption { type = lib.types.ints.between 1 300; default = 45; description = "Pending lifetime including suspend."; };
    maxPayloadBytes = lib.mkOption { type = lib.types.ints.between 1 1048576; default = 1048576; description = "Maximum UTF-8 submission size."; };
    backend = lib.mkOption {
      type = lib.types.enum [ "uinput" "mock" ]; default = "uinput";
      description = "Use mock only for isolated tests; mock never injects text.";
    };
    authorization = {
      storeUsers = authOption "Usernames allowed to submit text.";
      typeUsers = authOption "Usernames allowed to consume text through typing.";
      clearUsers = authOption "Usernames allowed to clear or cancel.";
      statusUsers = authOption "Usernames allowed to inspect metadata.";
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = lib.all (name: builtins.match "[a-zA-Z_][a-zA-Z0-9_.-]*[$]?" name != null) clientUsers;
      message = "services.viska authorization requires ordinary Unix usernames.";
    }];
    environment.systemPackages = [ cfg.package ];
    environment.etc."viska/config.json".source = configFile;
    users.groups.viskad = {};
    # Group access permits connecting only; daemon credentials enforce every role.
    users.groups.viska-clients.members = clientUsers;
    users.users.viskad = {
      isSystemUser = true; group = "viskad";
      extraGroups = [ "viska-clients" ];
      description = "Viska delivery service";
    };
    boot.kernelModules = lib.mkIf (cfg.backend == "uinput") [ "uinput" ];
    services.udev.extraRules = lib.mkIf (cfg.backend == "uinput") ''
      KERNEL=="uinput", SUBSYSTEM=="misc", OWNER="root", GROUP="viskad", MODE="0660", OPTIONS+="static_node=uinput"
    '';
    # The CLI also locks its stdin allocation. Users must log in again for PAM
    # limits/group changes to apply; bounded 4 MiB soft/hard limit, not CAP_IPC_LOCK.
    security.pam.loginLimits = map (name: {
      domain = name; type = "-"; item = "memlock"; value = "4096";
    }) cfg.authorization.storeUsers;
    systemd.services.viskad = {
      description = "Viska one-shot text delivery";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-udev-trigger.service" "nss-user-lookup.target" ];
      restartTriggers = [ configFile ];
      startLimitIntervalSec = 60;
      startLimitBurst = 5;
      serviceConfig = {
        Type = "notify";
        NotifyAccess = "main";
        User = "viskad";
        Group = "viska-clients";
        SupplementaryGroups = [ "viskad" ];
        ExecStart = "${cfg.package}/bin/viskad --config /etc/viska/config.json";
        RuntimeDirectory = "viska";
        RuntimeDirectoryMode = "0750";
        UMask = "0117";
        Restart = "on-failure";
        RestartSec = 2;
        TimeoutStartSec = 15;
        TimeoutStopSec = 5;
        LimitMEMLOCK = "8M";
        LimitCORE = 0;
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = false;
        DevicePolicy = "closed";
        DeviceAllow = lib.optional (cfg.backend == "uinput") "/dev/uinput w";
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RestrictRealtime = true;
        SystemCallArchitectures = "native";
        # NSS may use AF_UNIX services or network-backed resolvers. Keep NSS
        # functional rather than pretending all account sources are local files.
      };
    };
  };
}
