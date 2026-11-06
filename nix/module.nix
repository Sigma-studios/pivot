self:
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.pivot;

  # System-wide COSMIC shortcut overrides. COSMIC reads `custom` from ~/.config/cosmic/... and,
  # when the user has no such file, falls back to the first $XDG_DATA_DIRS/cosmic/... directory.
  # That is /run/current-system/sw/share/cosmic, where cosmic-comp's `defaults` also live.
  # Bindings in `custom` override the built-in WindowSwitcher bindings for the same keys.
  shortcuts = pkgs.writeTextDir "share/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom" ''
    {
        (modifiers: [Alt], key: "Tab"): Spawn("pivot next"),
        (modifiers: [Alt, Shift], key: "Tab"): Spawn("pivot prev"),
        (modifiers: [Super], key: "Tab"): Spawn("pivot next"),
        (modifiers: [Super, Shift], key: "Tab"): Spawn("pivot prev"),
    }
  '';
in
{
  options.programs.pivot = {
    enable = lib.mkEnableOption "pivot, a live-preview Alt+Tab switcher for COSMIC";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "pivot.packages.\${system}.default";
      description = "The pivot package to use.";
    };

    autostart = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Start the pivot daemon with the desktop session (XDG autostart). Without it the
        daemon is started on the first Alt+Tab, which makes that first press slower.
      '';
    };

    bindAltTab = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Bind Alt+Tab, Alt+Shift+Tab, Super+Tab and Super+Shift+Tab to pivot, replacing
        COSMIC's built-in switcher, by shipping a system-wide COSMIC `custom` shortcuts file.
        COSMIC only uses it while you have no personal shortcut file
        (~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom); see
        `patchUserShortcuts` for that case.
      '';
    };

    patchUserShortcuts = lib.mkOption {
      type = lib.types.bool;
      default = cfg.bindAltTab;
      defaultText = lib.literalExpression "config.programs.pivot.bindAltTab";
      description = ''
        If you already have a personal COSMIC shortcut file (created as soon as you change any
        shortcut in COSMIC Settings), it hides the system-wide one entirely. With this option the
        daemon adds the pivot bindings to that file at startup when they are missing, after
        backing it up next to it as `custom.bak-pivot`. Requires `autostart`.

        Turn it off if you manage shortcuts with cosmic-manager (declare the bindings there), and
        run `pivot remove-shortcuts` before uninstalling pivot so Alt+Tab isn't left pointing at
        a missing program.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ] ++ lib.optional cfg.bindAltTab shortcuts;
    environment.pathsToLink = [ "/share/cosmic" ];

    environment.etc."xdg/autostart/pivot.desktop" = lib.mkIf cfg.autostart {
      text = ''
        [Desktop Entry]
        Type=Application
        Name=Pivot window switcher
        Exec=${lib.getExe cfg.package} daemon${lib.optionalString cfg.patchUserShortcuts " --ensure-shortcuts"}
        NoDisplay=true
        OnlyShowIn=COSMIC;
      '';
    };
  };
}
