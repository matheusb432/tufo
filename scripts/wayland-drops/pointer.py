"""Drags the pointer in a GNOME Shell session through Mutter's RemoteDesktop D-Bus API.

    python3 pointer.py <drag source log> <from x> <from y> <to x> <to y>

Moves the pointer to the start until the drag source logs it there, since Mutter drops input
until its virtual device is ready. GTK asks to be activated when its window opens and winit
doesn't, so the maximized source covers Tufo; Alt+Tab brings Tufo forward until the source no
longer sees the pointer at the end. Then it presses the left button, glides to the end, releases
it, and waits for the source to log the drag's end. Coordinates are logical pixels on the only
monitor; the drag source is maximized, so its coordinates match the monitor's below its title
bar.
"""

import pathlib
import sys
import time

from gi.repository import Gio, GLib

BUS_NAME = "org.gnome.Mutter.RemoteDesktop"
SESSION_INTERFACE = "org.gnome.Mutter.RemoteDesktop.Session"
# linux/input-event-codes.h
BUTTON_LEFT = 0x110
# X11 keysymdef.h
ALT_KEYSYM = 0xFFE9
TAB_KEYSYM = 0xFF09
RAISE_ATTEMPTS = 5
# Far past any monitor, so the pointer clamps to the top-left corner.
TOP_LEFT = (-100_000.0, -100_000.0)
GLIDE_STEPS = 30
STEP_DELAY_SECONDS = 0.02
READY_ATTEMPTS = 20
READY_DELAY_SECONDS = 0.5
DRAG_END_TIMEOUT_SECONDS = 10.0

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)


def call(path, method, arguments=None, reply=None, interface=SESSION_INTERFACE):
    return bus.call_sync(
        BUS_NAME, path, interface, method, arguments, reply, Gio.DBusCallFlags.NONE, 5000, None
    )


def logged(log, text):
    return text in log.read_text()


def main():
    log = pathlib.Path(sys.argv[1])
    start_x, start_y, end_x, end_y = (float(value) for value in sys.argv[2:6])
    session = call(
        "/org/gnome/Mutter/RemoteDesktop",
        "CreateSession",
        reply=GLib.VariantType("(o)"),
        interface=BUS_NAME,
    ).unpack()[0]
    call(session, "Start")

    def move(dx, dy):
        call(session, "NotifyPointerMotionRelative", GLib.Variant("(dd)", (dx, dy)))
        time.sleep(STEP_DELAY_SECONDS)

    def button(pressed):
        call(session, "NotifyPointerButton", GLib.Variant("(ib)", (BUTTON_LEFT, pressed)))
        time.sleep(0.2)

    def move_to(x, y):
        move(*TOP_LEFT)
        move(x, y)
        time.sleep(READY_DELAY_SECONDS)

    def key(keysym, pressed):
        call(session, "NotifyKeyboardKeysym", GLib.Variant("(ub)", (keysym, pressed)))
        time.sleep(0.1)

    for _ in range(READY_ATTEMPTS):
        move_to(start_x, start_y)
        if logged(log, "pointer at "):
            break
    else:
        raise SystemExit("the drag source never saw the pointer")
    for _ in range(RAISE_ATTEMPTS):
        seen = len(log.read_text())
        move_to(end_x, end_y)
        if "pointer at " not in log.read_text()[seen:]:
            break
        key(ALT_KEYSYM, True)
        key(TAB_KEYSYM, True)
        key(TAB_KEYSYM, False)
        key(ALT_KEYSYM, False)
        time.sleep(1.0)
    else:
        raise SystemExit("Tufo's window never came in front of the drag source")
    move_to(start_x, start_y)
    button(True)
    for _ in range(GLIDE_STEPS):
        move((end_x - start_x) / GLIDE_STEPS, (end_y - start_y) / GLIDE_STEPS)
    time.sleep(0.5)
    button(False)
    deadline = time.monotonic() + DRAG_END_TIMEOUT_SECONDS
    while not logged(log, "drag ended"):
        if time.monotonic() > deadline:
            raise SystemExit("the drag never ended")
        time.sleep(0.1)
    call(session, "Stop")


if __name__ == "__main__":
    main()
