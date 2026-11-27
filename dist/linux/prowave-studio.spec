# Binary RPM for Fedora/openSUSE/RHEL 9+ (x86_64, glibc 2.31+). It repackages the release
# binary built by CI instead of compiling, so build it with:
#   rpmbuild -bb dist/linux/prowave-studio.spec --define "_sourcedir <dir with prowave and
#   the dist/linux files>" --define "pkg_version <version>"
# Run-time library dependencies (libasound.so.2, liblilv-0.so.0, libc) are found automatically from the ELF.
%global debug_package %{nil}
%global __strip /bin/true
%global _build_id_links none

Name:           prowave-studio
Version:        %{pkg_version}
Release:        1
Summary:        Music production studio in the style of Logic Pro
License:        GPL-3.0-or-later
URL:            https://prowave.invalid/
ExclusiveArch:  x86_64
Source0:        prowave
Source1:        prowave-studio.desktop
Source2:        prowave.png
Source3:        prowave-mime.xml
Source4:        README.txt

# Windowing, GPU and font libraries are loaded at run time, so they are not found from
# the ELF; recommend them instead (Fedora package names).
Recommends:     libxkbcommon-x11
Recommends:     libwayland-client
Recommends:     mesa-libEGL
Recommends:     fontconfig
Recommends:     xdg-desktop-portal
Suggests:       mesa-vulkan-drivers
Suggests:       zenity

%description
Pro Wave is a digital audio workstation written in Rust: tracks, regions, a piano
roll, a mixer and a built-in synthesizer, with audio through ALSA (PipeWire and PulseAudio
via their ALSA plug-ins). This is an early development build.

%prep

%build

%install
install -D -m 755 %{SOURCE0} %{buildroot}%{_bindir}/prowave
install -D -m 644 %{SOURCE1} %{buildroot}%{_datadir}/applications/prowave-studio.desktop
install -D -m 644 %{SOURCE2} %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/prowave.png
install -D -m 644 %{SOURCE3} %{buildroot}%{_datadir}/mime/packages/prowave.xml
install -D -m 644 %{SOURCE4} %{buildroot}%{_docdir}/%{name}/README.txt

%files
%{_bindir}/prowave
%{_datadir}/applications/prowave-studio.desktop
%{_datadir}/icons/hicolor/512x512/apps/prowave.png
%{_datadir}/mime/packages/prowave.xml
%doc %{_docdir}/%{name}/README.txt
