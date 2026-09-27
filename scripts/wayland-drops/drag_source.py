"""A maximized GTK window that drags the files named on its command line.

Pressing anywhere in it and moving the pointer starts a copy drag of those files, which GTK
offers as `text/uri-list` among others. It prints where each drag started and how it ended.
"""

import sys

import gi

gi.require_version("Gdk", "4.0")
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk  # noqa: E402


def open_window(paths):
    print("drag source opened", flush=True)
    window = Gtk.Window(title="Tufo drag source")
    area = Gtk.Label(label="Drag these files onto Tufo", hexpand=True, vexpand=True)
    drag = Gtk.DragSource(actions=Gdk.DragAction.COPY)
    files = Gdk.FileList.new_from_array([Gio.File.new_for_path(path) for path in paths])

    def prepare(_source, x, y):
        print(f"drag started at {x:.0f},{y:.0f}", flush=True)
        return Gdk.ContentProvider.new_for_value(files)

    def ended(_source, _drag, _delete):
        print("drag ended", flush=True)

    drag.connect("prepare", prepare)
    drag.connect("drag-end", ended)
    area.add_controller(drag)
    pointer = Gtk.EventControllerMotion()
    for signal in ("enter", "motion"):
        pointer.connect(signal, lambda _controller, x, y: print(f"pointer at {x:.0f},{y:.0f}", flush=True))
    area.add_controller(pointer)
    window.set_child(area)
    window.maximize()
    window.present()


def main():
    # A plain window and main loop: GtkApplication waits on desktop portals, which a headless
    # test session lacks.
    Gtk.init()
    open_window(sys.argv[1:])
    GLib.MainLoop().run()


if __name__ == "__main__":
    main()
