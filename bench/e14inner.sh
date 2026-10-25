#!/usr/bin/env bash
# Inside the private session bus: start the AT-SPI bus, mark accessibility
# on, then run Cage with the app as its client. usage: e14inner.sh APP
set -u
E=/var/tmp/glarb-bench/e14
app=$1
echo "$DBUS_SESSION_BUS_ADDRESS" > "$XDG_RUNTIME_DIR/dbus.addr"
"$ATSPI_LIBEXEC/at-spi-bus-launcher" --launch-immediately &
for _ in $(seq 50); do
  gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress > /dev/null 2>&1 && break
  sleep 0.1
done
gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
  --method org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled \
  '<true>' > /dev/null 2>&1
exec cage -- "$E/e14run.sh" "$app"
