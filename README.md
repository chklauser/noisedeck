Noisedeck is a controller for the Elgato Stream Deck. Unlike the official controller software (Windows and MacOs only), Noisedeck focuses on just a single use case: play ambient sound/background music. This singular focus lets me make the UI more reactive.

At this stage, the software is not really ready for you to use. I'm dog-fooding it in my own pen&paper RPG sessions right now.

# Build
## Prerequisites
### Fedora Linux
```bash
# Development Dependencies (systemd packages libuv)
dnf install alsa-lib-devel systemd-devel
```