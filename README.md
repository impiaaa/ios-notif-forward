# ![icon](https://github.com/impiaaa/ios-notif-forward/assets/1307275/6607cb68-13b0-406d-95ed-2e4799943d24) ios-notif-forward

Forward notifications from Apple devices to your desktop over Bluetooth, with a simple desktop tray app.

**[Download Here](/impiaaa/ios-notif-forward/releases/latest)**

## Additional Dependencies for Linux/Other Unix

### Arch Linux/Manjaro

`sudo pacman -S gtk3 xdotool libappindicator-gtk3 # or libayatana-appindicator`

### Debian/Ubuntu

`sudo apt install libgtk-3-dev libxdo-dev libayatana-appindicator3-dev # or libappindicator3-dev`

## Installation

The app should run fine from wherever. However, on Linux or other Unix, I recommend installing the package files, e.g. with `sudo cp -r bin share /usr/local/`. On any system, I recommend setting the app to automatically run on desktop user login.

## Running

This requires a computer with Bluetooth LE capability. Pair your phone as normal for your system. The app will automatically start receiving notifications from any compatible device that connects to the computer. I've had the best luck when initiating the connection from the phone rather than from the computer. You will need to grant permission for your computer to receive system notifications the first time using the app. I haven't been able to succesfully test on Windows or Mac. To close the app and stop receiving notifications, choose "Quit" from the app's tray menu.
