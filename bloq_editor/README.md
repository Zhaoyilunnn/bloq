# bloq_editor

Bevy/egui editor for constructing and inspecting block graphs on desktop or
WASM/WebGPU. The hosted build is at [bloqec.com/editor/](https://bloqec.com/editor/).

```bash
just editor # native
just web    # browser
```

Desktop builds embed the Bloq symbol as the window/taskbar icon on Windows and
X11 and the Dock icon on macOS. Windows executables also embed an `.ico` resource.
For a macOS `.app` or Linux application launcher, run `just editor-package`
(Python 3.11+). The archive and SHA256 file are written to `target/desktop`.
On macOS, extract it and move **Bloq Editor.app** to Applications. On Linux,
extract it and run `python3 install.py` to install the application and icon for
the current user. Its launcher matches the editor's Wayland application ID.

The source icon is [`assets/icons/bloq.png`](assets/icons/bloq.png). After replacing
it, run `just editor-icons` to regenerate the PNG, ICO, and ICNS variants with
Pillow. `just editor-package-check` verifies both bundle layouts and the Linux
installer without installing into your desktop.

Open a BLOG file or gallery example, edit its geometry, then use **Compile &
export** or **Program** to inspect the compiled circuit. **Modules** supports
reusable definitions and named compositions. The editor does not simulate physical circuits.

Module save and undo retain the hierarchy. Edit definitions through linked tabs.
The authored source is one hierarchy-capable `BlockGraph`, shared with the CLI
and library compiler input. Composed viewport geometry is an explicit flat
preview; compilation uses the authored graph.
The composition preview is not directly editable. Viewer branch pins select a
circuit view without changing the source document.

The browser build autosaves sessions.
Runtime jobs, caches, and undo history start fresh on reload. Session layouts
are versioned independently of hosted documentation.

Native builds can render a BLOG graph to PNG:

```bash
cargo run -p bloq_editor --release -- --render-blog graph.blog --output figure.png
```

Rendering requires a GPU and display.
