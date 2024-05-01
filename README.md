# {{project-name}}

Install:

```
cargo install espflash espup ldproxy
```

Initialize:

```
. $HOME/export-esp.sh
```

Build:

```
CRATE_CC_NO_DEFAULTS=1 cargo build
cargo run -- -p /dev/tty.usbserial-10
```


Create firmware:

```
export CRATE_CC_NO_DEFAULTS=1
cargo build --release
espflash save-image --chip esp32 -s 4mb target/xtensa-esp32-espidf/debug/{{project-name}} target/firmware.bin
```

Upload `firmware.bin` using <<ESP32_HOST>>/firmware.