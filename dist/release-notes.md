**Pro Wave @VERSION@** is an early development build of a Logic Pro-style music production app (DAW) written in Rust: tracks, MIDI and audio regions, Piano Roll, Mixer, Library and a built-in synthesizer. Expect missing features and rough edges.

@CHANGES@

## Downloads

| System | File | How to install |
|---|---|---|
| Windows 10/11 x64 | `prowave-windows-x86_64.zip` | Unzip, run `prowave.exe` |
| macOS (Apple Silicon + Intel) | `prowave-macos-universal.zip` | Unzip, move `Pro Wave.app` to Applications (see below) |
| Debian, Ubuntu, Mint | `prowave-studio_@VERSION@_amd64.deb` | `sudo apt install ./prowave-studio_@VERSION@_amd64.deb` |
| Fedora, openSUSE, RHEL 9+ | `prowave-studio-@VERSION@-1.x86_64.rpm` | `sudo dnf install ./prowave-studio-@VERSION@-1.x86_64.rpm` |
| Any Linux | `Pro_Wave-@VERSION@-x86_64.AppImage` | `chmod +x` it and run it |
| Any Linux (sandboxed) | `prowave-studio-@VERSION@-x86_64.flatpak` | `flatpak install --user ./prowave-studio-@VERSION@-x86_64.flatpak` |
| Any Linux | `prowave-linux-x86_64.tar.gz` | Unpack, run `./prowave` or `./install.sh` |

Linux packages need glibc 2.31 or newer (Ubuntu 20.04+, Debian 11+, Fedora 32+) and ALSA for sound. Details, required libraries and troubleshooting are in `README.txt` inside the tarball and in [`dist/README-linux.txt`](https://github.com/fabox-os/Pro-Wave/blob/@TAG@/dist/README-linux.txt).

**Plug-ins:** the macOS Apple Silicon and Windows x64 packages include the sandboxed plug-in host (`prowave-plugin-helper`) and load VST3, Audio Unit and CLAP plug-ins on the Stereo Out. Linux and Intel Macs do not host plug-ins yet.

## Not signed yet

None of the files is code signed, so your system will warn before running them:

- **Windows:** SmartScreen shows "Windows protected your PC". Choose *More info*, then *Run anyway*.
- **Windows antivirus:** Norton, Avast, AVG or McAfee may block the unsigned `prowave.exe` with "Windows cannot access the specified device, path or file". Add the Pro Wave folder to the antivirus's exclusions (Norton: *Settings > Antivirus > Scans and Risks > Items to Exclude from Auto-Protect, Script Control, SONAR and Download Intelligence Detection*).
- **macOS:** the app is ad-hoc signed but not notarized. The first time, right-click `Pro Wave.app`, choose *Open*, then *Open* again. If macOS says the app is damaged, run `xattr -dr com.apple.quarantine "/Applications/Pro Wave.app"`.

## How well tested is it?

Windows is the developer's daily platform. The Linux and macOS packages were built and install-checked on GitHub's build servers (Ubuntu and Fedora installs, and a 15-second launch under a virtual display on Linux), but have **not yet been run on real Linux or macOS hardware**. Please [report what works and what doesn't](https://github.com/fabox-os/Pro-Wave/issues).

Licensed under the GNU General Public License v3 or later.
