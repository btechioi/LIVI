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
          allowUnfree = true;
          android_sdk.accept_license = true;
        };
      };

      androidComposition = pkgs.androidenv.composeAndroidPackages {
        platformVersions = [ "35" ];
        buildToolsVersions = [ "35.0.0" ];
        includeNDK = true;
        ndkVersions = [ "27.2.12479018" ];
        includeCmake = true;
        cmakeVersions = [ "3.22.1" ];
        includeEmulator = true;
        includeSystemImages = true;
        systemImageTypes = [ "google_apis" ];
        abiVersions = [ "x86_64" ];
      };

      androidSdk = androidComposition.androidsdk;
    in {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          # JavaScript / TypeScript
          nodejs_24
          pnpm

          # Flutter
          flutter
          jdk17

          # Android
          androidSdk

          # Rust
          rustc
          cargo
          rustfmt
          clippy

          # Native build tooling
          pkg-config
          cmake
          gcc
          gnumake
          git

          # GStreamer
          gst_all_1.gstreamer
          gst_all_1.gst-plugins-base

          # Wayland / compositor
          wayland
          libxkbcommon

          # Bluetooth / networking
          bluez
          pipewire
          hostapd
          dnsmasq
          iw
          avahi

          # Audio / filesystem / USB
          pulseaudio
          fuse3
          libusb1
        ];

        ANDROID_HOME = "${androidSdk}/libexec/android-sdk";
        ANDROID_SDK_ROOT = "${androidSdk}/libexec/android-sdk";

        shellHook = ''
          export PATH="${pkgs.pnpm}/bin:${pkgs.nodejs_24}/bin:$PATH"

          export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/tools/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/emulator:$PATH"

          export PKG_CONFIG_PATH="${pkgs.gst_all_1.gstreamer.dev}/lib/pkgconfig:${pkgs.gst_all_1.gst-plugins-base.dev}/lib/pkgconfig:$PKG_CONFIG_PATH"

          echo "Entering LIVI development shell."
          echo "Node.js: $(node -v)"
          echo "pnpm: $(pnpm -v)"
          echo "Rust: $(rustc --version)"
          echo "Cargo: $(cargo --version)"
          echo "Flutter: $(flutter --version | head -n 1)"
          echo "Java: $(java --version 2>&1 | head -n 1)"
          echo "ADB: $(adb version | head -n 1)"
          echo "GStreamer: $(gst-launch-1.0 --version | head -n 1)"
        '';
      };
    };
}
