{
  description = "LIVI - Linux In-Vehicle Infotainment development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        config = {
          allowUnfree = true; # Allow unfree packages, e.g., for some fonts or tools
        };
      };
    in {
      devShells.${system}.default = pkgs.mkShell {
        # Core development tools
        packages = with pkgs; [
          nodejs_20 # Aligned with project's package.json, Node.js 24.x from README can be used if needed
          flutter # For livi_sensors project
          pnpm
          rust-bin.stable.latest.default # Stable Rust toolchain
          cargo
          rustfmt
          clippy
          pkg-config
          cmake
          git
          # Build essentials for Linux
          gcc
          gnumake
          # GStreamer and its plugins for livi-gst-video
          gstreamer
          gst-plugins-base
          # Wayland and XKBCommon for livi-compositor
          wayland
          libxkbcommon
          # Runtime packages for native CarPlay and wireless Android Auto
          bluez
          pipewire # Replaces libspa-0.2-bluetooth for Fedora/NixOS
          hostapd
          dnsmasq
          iw
          rfkill
          avahi
          pulseaudio
          fuse3
          libusb1
        ];

        # Environment variables for Rust and GStreamer
        shellHook = ''
          export NIX_LD_PATH=${pkgs.lib.makeLibraryPath pkgs.lib.getBuildInputs}
          export PKG_CONFIG_PATH="${pkgs.gstreamer.dev}/lib/pkgconfig:${pkgs.gst-plugins-base.dev}/lib/pkgconfig:$PKG_CONFIG_PATH"
          echo "Entering LIVI development shell."
          echo "Node.js: $(node -v)"
          echo "pnpm: $(pnpm -v)"
          echo "Rust: $(rustc -v)"
        '';
      };
    };
}
