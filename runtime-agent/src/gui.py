"""Cloud-desktop launcher. No network service, shell commands, or client file paths."""
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
from urllib.parse import unquote, urlsplit


def dropped_paths(uris):
    paths = []
    for uri in uris:
        parsed = urlsplit(uri)
        if parsed.scheme != "file" or parsed.netloc not in ("", "localhost"):
            raise ValueError("Drop a file from this cloud PC, not a web or network link.")
        path = unquote(parsed.path)
        if not os.path.isabs(path) or any(c in path for c in "\r\n\0"):
            raise ValueError("The application must have an absolute local file path.")
        paths.append(path)
    if len(paths) != 1:
        raise ValueError("Drop one application at a time.")
    return paths


def engine_call(engine, *args):
    result = subprocess.run([engine, *args], capture_output=True, text=True, timeout=240)
    if result.returncode:
        try:
            message = json.loads(result.stderr).get("error", result.stderr)
        except (ValueError, AttributeError):
            message = result.stderr.strip() or "Runtime operation failed."
        raise RuntimeError(message)
    return json.loads(result.stdout)


def main(engine):
    import gi
    gi.require_version("Gtk", "3.0")
    from gi.repository import Gdk, GLib, Gtk

    class Launcher(Gtk.Window):
        def __init__(self):
            super().__init__(title="No Land Runtime")
            self.set_default_size(560, 450)
            self.set_border_width(20)
            self.connect("destroy", Gtk.main_quit)
            self.busy = False
            self.apps = []
            self.last_launch = None
            box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
            self.add(box)
            title = Gtk.Label(label="Run a Linux app in its own runtime")
            title.get_style_context().add_class("title")
            box.pack_start(title, False, False, 0)
            self.drop = Gtk.EventBox()
            self.drop.set_visible_window(True)
            self.drop.set_size_request(-1, 110)
            self.drop.add(Gtk.Label(label="Drop an AppImage or Linux executable here"))
            self.drop.drag_dest_set(Gtk.DestDefaults.ALL, [], Gdk.DragAction.COPY)
            self.drop.drag_dest_add_uri_targets()
            self.drop.connect("drag-data-received", self.dropped)
            box.pack_start(self.drop, False, False, 0)
            browse = Gtk.Button(label="Choose an app…")
            browse.connect("clicked", self.browse)
            box.pack_start(browse, False, False, 0)
            hint = Gtk.Label(label="Choose files on this cloud PC. Windows .exe files are not supported.")
            hint.set_line_wrap(True)
            box.pack_start(hint, False, False, 0)
            self.list = Gtk.ListBox()
            self.list.set_selection_mode(Gtk.SelectionMode.NONE)
            scroll = Gtk.ScrolledWindow()
            scroll.set_min_content_height(100)
            scroll.add(self.list)
            box.pack_start(scroll, True, True, 0)
            self.spinner = Gtk.Spinner()
            box.pack_start(self.spinner, False, False, 0)
            self.status = Gtk.Label(label="Checking the provisioned runtime…")
            self.status.set_line_wrap(True)
            self.status.set_selectable(True)
            box.pack_start(self.status, False, False, 0)
            warning = Gtk.Label(label="Separate app data; shared display, audio and network.\nTrusted apps only — not a security sandbox.")
            warning.set_line_wrap(True)
            box.pack_start(warning, False, False, 0)
            tools = Gtk.ButtonBox()
            for label, callback in [("Diagnostics", self.diagnostics), ("Refresh", self.refresh)]:
                button = Gtk.Button(label=label)
                button.connect("clicked", callback)
                tools.add(button)
            box.pack_start(tools, False, False, 0)
            self.connect("delete-event", self.closing)
            self.show_all()
            self.refresh()

        def closing(self, *_):
            # Closing the launcher never stops an already launched app.
            return False

        def message(self, message, error=False):
            dialog = Gtk.MessageDialog(transient_for=self, modal=True,
                message_type=Gtk.MessageType.ERROR if error else Gtk.MessageType.INFO,
                buttons=Gtk.ButtonsType.OK, text="No Land Runtime")
            dialog.format_secondary_text(str(message))
            dialog.run()
            dialog.destroy()

        def work(self, text, action, complete=None):
            if self.busy:
                return
            self.busy = True
            self.spinner.start()
            self.status.set_text(text)

            def finished(result, error):
                self.busy = False
                self.spinner.stop()
                if error:
                    self.status.set_text(str(error))
                    self.message(error, error=True)
                elif complete:
                    complete(result)
                return False

            def run():
                try:
                    result = action()
                    GLib.idle_add(finished, result, None)
                except Exception as error:
                    GLib.idle_add(finished, None, error)
            threading.Thread(target=run, daemon=True).start()

        def refresh(self, *_):
            def done(report):
                self.apps = report.get("apps", [])
                for row in self.list.get_children():
                    self.list.remove(row)
                for app in self.apps:
                    row = Gtk.Box(spacing=8)
                    label = Gtk.Label(label=app["name"], xalign=0)
                    label.set_ellipsize(3)
                    row.pack_start(label, True, True, 0)
                    button = Gtk.Button(label="Run")
                    button.connect("clicked", lambda _, item=app: self.launch(item))
                    row.pack_end(button, False, False, 0)
                    self.list.add(row)
                self.list.show_all()
                self.status.set_text((self.last_launch or "Ready — drop an app to run it.") if report.get("installed")
                    else "Runtime not ready. Resume Provisioning in Noland Connect to install or repair it.")
            self.work("Checking runtime…", lambda: engine_call(engine, "status", "--json"), done)

        def browse(self, *_):
            if self.busy:
                return
            dialog = Gtk.FileChooserDialog(title="Choose a Linux app on this cloud PC", parent=self,
                action=Gtk.FileChooserAction.OPEN)
            dialog.add_buttons(Gtk.STOCK_CANCEL, Gtk.ResponseType.CANCEL, "Run", Gtk.ResponseType.OK)
            if dialog.run() == Gtk.ResponseType.OK:
                path = dialog.get_filename()
            else:
                path = None
            dialog.destroy()
            if path:
                self.open_app(path)

        def dropped(self, _, context, x, y, selection, info, timestamp):
            try:
                if self.busy:
                    raise ValueError("Wait for the current launch to finish checking.")
                path = dropped_paths(selection.get_uris() or [])[0]
                Gtk.drag_finish(context, True, False, timestamp)
                self.open_app(path)
            except Exception as error:
                Gtk.drag_finish(context, False, False, timestamp)
                self.message(error, error=True)

        def open_app(self, path):
            dialog = Gtk.MessageDialog(transient_for=self, modal=True,
                message_type=Gtk.MessageType.QUESTION, buttons=Gtk.ButtonsType.OK_CANCEL,
                text="Run this trusted application?")
            dialog.format_secondary_text(f"{path}\n\nIt will share your display and audio. Only run apps you trust.")
            approved = dialog.run() == Gtk.ResponseType.OK
            dialog.destroy()
            if not approved:
                return

            def run():
                # Validate/register before making any permission changes. The CLI
                # only grants the owner's executable bit for a selected ELF file.
                app = engine_call(engine, "register", "--file", path, "--make-executable")
                return engine_call(engine, "launch", "--app-id", app["id"])
            self.work("Checking graphics, audio and runtime; starting app…", run, self.launched)

        def launch(self, app):
            self.work("Checking runtime; starting app…",
                lambda: engine_call(engine, "launch", "--app-id", app["id"]), self.launched)

        def launched(self, result):
            self.last_launch = f"Launch requested. Log: {result.get('log', 'see diagnostics')}"
            self.status.set_text(self.last_launch)
            # Refresh the list without replacing the launch result with a false
            # claim that the application successfully opened.
            self.refresh()

        def diagnostics(self, *_):
            self.work("Checking graphics, audio and display…",
                lambda: engine_call(engine, "doctor", "--json"),
                lambda report: self.message("\n".join(
                    ("OK: " if item["ok"] else "FAILED: ") + item["name"] + " — " + item["detail"]
                    for item in report.get("checks", []))))

    Launcher()
    Gtk.main()


if __name__ == "__main__":
    main(sys.argv[1])
