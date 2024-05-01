# {{project-name}}

Install:

```
cargo install espflash espup ldproxy
```

Initialize:

```
. $HOME/export-esp.sh
```

Build (flash to connected device):

```
export CRATE_CC_NO_DEFAULTS=1
export WIFI_SSID=<<WIFI_SSID>> WIFI_PASS=<<WIFI_PASS>>
cargo run -- -p /dev/tty.usbserial-10 # only for Mac
# or
cargo run # to select serial interface from the list
```


Create firmware:

```
export CRATE_CC_NO_DEFAULTS=1
export WIFI_SSID=<<WIFI_SSID>> WIFI_PASS=<<WIFI_PASS>>
cargo build --release
espflash save-image --chip esp32 -s 4mb target/xtensa-esp32-espidf/debug/{{project-name}} target/firmware.bin
```

Upload `firmware.bin` using <<ESP32_HOST>>/firmware.