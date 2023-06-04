# <picture><source media="(prefers-color-scheme: dark)" srcset="icon-32-white.png"><source media="(prefers-color-scheme: light)" srcset="icon-32-black.png"><img alt="icon" src="icon-32-black.png"></picture> ios-notif-forward

Forward notifications from Apple mobile devices to your desktop over Bluetooth, with a simple desktop tray app.

<a target="_blank" rel="noopener noreferrer" href="https://user-images.githubusercontent.com/1307275/243147241-449e2fb0-d1ba-4705-9059-676939a14ec7.jpeg"><img src="https://user-images.githubusercontent.com/1307275/243147241-449e2fb0-d1ba-4705-9059-676939a14ec7.jpeg" alt="Screenshot of iOS notification" style="max-width: 100%; width: 515px"></a> ![Screenshot of Gnome notification](https://github.com/impiaaa/ios-notif-forward/assets/1307275/28c55151-f506-4105-80cb-6775455e87e3)

**[Download Here](https://github.com/impiaaa/ios-notif-forward/releases/latest)**

## Additional Dependencies for Linux/Other Unix

### Arch Linux/Manjaro

`sudo pacman -S gtk3 xdotool libappindicator-gtk3 # or libayatana-appindicator`

### Debian/Ubuntu

`sudo apt install libgtk-3 libxdo libayatana-appindicator3 # or libappindicator3`

## Installation

The app should run fine from wherever. However, on Linux or other Unix, I recommend installing the package files, e.g. with `sudo cp -r bin share /usr/local/`. On any system, I recommend setting the app to automatically run on desktop user login.

## Running

This requires a computer with Bluetooth LE capability.

1. Pair your device over Bluetooth as normal for your system.
2. The app will automatically start receiving notifications from any compatible device that connects to the computer.
3. You will need to grant permission from your device for your computer to receive system notifications the first time you use the app.
4. To close the app and stop receiving notifications, choose "Quit" from the app's tray menu.

I've had the best luck when initiating the connection from the device rather than from the computer. I haven't been able to succesfully test on Windows or Mac.

## Compile from Source

1. Clone the repository.
2. Install the "rust" package from your system's package manager.
3. Install the development packages for the dependencies if necessary.
4. Run `cargo run` to run a debug build. Run `cd src/package` and then `cargo run` to generate a release package.
