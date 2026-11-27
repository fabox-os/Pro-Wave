Pro Wave — Linux x86_64 build
=============================

Pro Wave is a DAW (music production app) in the style of Apple Logic Pro, written in
Rust. This is an early development build: expect missing features and rough edges.

NOTE: this build has not been tested on a real Linux machine yet (it was compiled either
from Windows or on GitHub's build servers). Please report what works and what doesn't (see
"Reporting problems" below).


1. System requirements
----------------------

- 64-bit x86 PC (x86_64 / amd64). ARM (Raspberry Pi, Apple Silicon Linux) is not supported.
- glibc 2.31 or newer: Ubuntu 20.04+, Debian 11+, Fedora 32+, Linux Mint 20+, Pop!_OS 20.04+,
  openSUSE Leap 15.3+, Arch/Manjaro (current). Check yours with:  ldd --version
- A graphics driver with Vulkan (preferred) or OpenGL ES 3 / EGL. Any recent Intel, AMD or
  NVIDIA driver works; very old GPUs or VMs may need the OpenGL fallback (see section 6).
- An X11 or Wayland desktop session (GNOME, KDE Plasma, Xfce, Cinnamon, Sway, ...).
- Sound through ALSA. PipeWire and PulseAudio systems work through their ALSA plug-in.


2. Install the libraries it needs
---------------------------------

Most desktop installs already have these. If the app doesn't start, install them:

Ubuntu / Debian / Mint / Pop!_OS:
  sudo apt install libasound2 liblilv-0-0 libxkbcommon-x11-0 libwayland-client0 libfontconfig1 \
                   libdbus-1-3 libx11-6 libxcursor1 libxrandr2 libxi6 libegl1 \
                   mesa-vulkan-drivers xdg-desktop-portal-gtk
  (Ubuntu 24.04+ / Debian 13+: if apt says libasound2 is a virtual package, use libasound2t64.)

Fedora:
  sudo dnf install alsa-lib lilv-libs libxkbcommon-x11 libwayland-client fontconfig dbus-libs \
                   libX11 libXcursor libXrandr libXi mesa-libEGL mesa-vulkan-drivers \
                   xdg-desktop-portal-gtk

Arch / Manjaro / EndeavourOS:
  sudo pacman -S alsa-lib lilv libxkbcommon-x11 wayland fontconfig dbus libx11 libxcursor \
                 libxrandr libxi libglvnd vulkan-icd-loader xdg-desktop-portal-gtk

openSUSE:
  sudo zypper install alsa liblilv-0-0 libxkbcommon-x11-0 libwayland-client0 fontconfig libdbus-1-3 \
                      xdg-desktop-portal-gtk

What each one is for:
  ALSA (libasound)        audio output
  xkbcommon, wayland, X11 the window and keyboard
  fontconfig              finding system fonts
  dbus + xdg-desktop-portal  the Open / Save file dialogs
  zenity                  fallback for the Open / Save dialogs when no desktop portal is
                          running (the "Save changes?" question is drawn by ProWave itself)
  Vulkan / EGL            drawing the interface on the GPU

On KDE you can use xdg-desktop-portal-kde instead of -gtk for KDE-style file dialogs.


3. Run it
---------

  tar -xzf prowave-linux-x86_64.tar.gz
  cd prowave-linux-x86_64
  ./prowave

If you get "Permission denied":  chmod +x prowave

Command-line options:
  ./prowave song.prowave             open a project
  ./prowave drums.wav bass.flac      start a new project and import audio files
  ./prowave song.prowave vocal.wav   open a project and import files into it
  ./prowave --play                   start playing right away
  ./prowave --piano                  open the Piano Roll on start

Audio files supported: WAV, AIFF, FLAC. (MP3 and CAF are not supported yet.)
You can also drag audio files from your file manager onto the window.


4. Install for your user (optional)
-----------------------------------

The easy way (no root needed): from the unpacked folder run

  ./install.sh

It copies the program to ~/.local/bin, adds Pro Wave to your app menu with an icon, and
makes double-clicking a .prowave file open it. Undo it with:  ./install.sh --uninstall
If your terminal can't find "prowave" afterwards, add ~/.local/bin to your PATH.

Files in this package: prowave (program), install.sh, prowave-studio.desktop (menu entry),
prowave.png (icon), prowave-mime.xml (.prowave file type).

Other package formats (same program, from the same release page):

  Debian / Ubuntu / Mint (.deb, installs to /usr):
    sudo apt install ./prowave-studio_<version>_amd64.deb
    remove with:  sudo apt remove prowave-studio

  Fedora / openSUSE / RHEL 9+ (.rpm):
    sudo dnf install ./prowave-studio-<version>-1.x86_64.rpm     (openSUSE: sudo zypper install ...)
    remove with:  sudo dnf remove prowave-studio

  AppImage (any distribution, nothing to install):
    chmod +x Pro_Wave-<version>-x86_64.AppImage
    ./Pro_Wave-<version>-x86_64.AppImage
    If it says FUSE is missing, install libfuse2 (Ubuntu 24.04+: libfuse2t64) or run it with
    APPIMAGE_EXTRACT_AND_RUN=1. It uses the system's audio and graphics libraries (section 2).

  Flatpak (sandboxed; needs Flathub for the runtime):
    flatpak install --user ./prowave-studio-<version>-x86_64.flatpak
    flatpak run studio.prowave.Studio
    The sandbox can read and write your home folder and plays sound through PulseAudio/
    PipeWire. Remove with:  flatpak uninstall studio.prowave.Studio

----------------------------------------------------------------------------------------------

5. Keyboard shortcuts
---------------------

Linux uses Ctrl where macOS uses Cmd:
  Ctrl+N new    Ctrl+O open    Ctrl+S save    Ctrl+Shift+S save as
  Space play/stop    Ctrl+R repeat region    Delete delete selection
  Y Library    I Inspector    X Mixer    Q quantize (Piano Roll)


6. Troubleshooting
------------------

The window doesn't open / GPU errors:
  Force OpenGL instead of Vulkan:     WGPU_BACKEND=gl ./prowave
  Use the integrated GPU on laptops:  WGPU_POWER_PREF=low ./prowave
  Check Vulkan works:                 vulkaninfo --summary   (package vulkan-tools)

Wayland problems (blurry, crashes, missing title bar or icon):
  Run it through XWayland instead:    WAYLAND_DISPLAY= ./prowave

No sound:
  - If ProWave can't open any audio device it still starts, and shows "No sound: ..." in the
    menu bar. It tries the default device first, then every other output device.
  - ProWave plays to ALSA's "default" device.
  - PipeWire (Ubuntu 22.10+, Fedora 34+): install pipewire-alsa (Debian/Ubuntu/Arch) or
    pipewire-alsa / alsa-plugins-pipewire (Fedora) so ALSA goes through PipeWire.
  - PulseAudio: install libasound2-plugins (Debian/Ubuntu) or alsa-plugins-pulseaudio
    (Fedora/Arch).
  - Check ALSA itself works:  speaker-test -c 2 -t sine   (Ctrl+C to stop)
  - JACK is not supported directly yet; use PipeWire's JACK/ALSA bridge.

Real-time audio priority (fewer dropouts):
  ProWave asks Linux to run its audio thread with real-time priority. This needs permission:
  - Easiest: join the audio group:  sudo usermod -aG audio $USER   (then log out and in)
    and make sure /etc/security/limits.d/ gives that group rtprio (Ubuntu Studio, Fedora's
    realtime-setup, and Arch's realtime-privileges package do this for you).
  - Or run under a system with rtkit (most desktops with PipeWire/PulseAudio have it).
  - If the system refuses, ProWave still plays at normal priority and logs a warning
    (see RUST_LOG=debug); you may just hear more crackles under heavy load.
  Check your limit:  ulimit -r   (a value of 0 means real-time is not allowed)

Crackles / dropouts:
  Watch the CPU meter in the LCD display. Close other heavy apps. On PipeWire you can ask
  for a bigger buffer:  PIPEWIRE_LATENCY=512/48000 ./prowave

Open/Save dialog doesn't appear:
  Install xdg-desktop-portal plus a backend (-gtk, -kde or -gnome) and log out and back in.
  If no portal is available, zenity is used instead, so installing zenity also helps.

"version `GLIBC_2.xx' not found":
  Your distribution is older than the requirement in section 1.

"error while loading shared libraries: libXXX.so":
  Install the missing library from section 2. To see everything that's missing:
    ldd ./prowave | grep "not found"


7. Logs, crash reports and temporary files
------------------------------------------

Logs go to the terminal. More detail:   RUST_LOG=debug ./prowave
Only ProWave's own messages:            RUST_LOG=prowave=debug ./prowave

If ProWave crashes, it prints the path of a crash report (a .toml file in /tmp, or in
$TMPDIR if set). Please attach that file when reporting the problem.

When you open a .prowave project, its audio is unpacked to /tmp/prowave-media/<process id>/.
This folder is removed when ProWave closes; leftovers from crashes are removed automatically
after 7 days.


8. Known limitations
--------------------

- Early build: Undo/Redo, Copy/Paste, recording and MIDI input are not done yet; many menu
  entries are shown disabled.
- Effects (EQ, Compressor, ...) are shown but don't process sound yet; instruments use a
  built-in synthesizer, not sampled sounds.
- Not tested on Linux hardware yet (see the note at the top).
- Audio goes through ALSA only (PipeWire/PulseAudio via their ALSA plug-ins); native JACK and
  PipeWire back-ends are not built in. Latency is whatever the ALSA default device gives.


9. Reporting problems
---------------------

Please include:
  - Distribution and version:   cat /etc/os-release
  - Desktop and session type:   echo $XDG_CURRENT_DESKTOP $XDG_SESSION_TYPE
  - GPU:                        lspci | grep -i -E "vga|3d"
  - Audio system:               pactl info | grep "Server Name"   (PipeWire or PulseAudio)
  - The terminal output of:     RUST_LOG=debug ./prowave
  - The crash report file, if there was one
