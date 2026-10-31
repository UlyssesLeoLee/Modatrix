# Bevy desktop shell

This is a standalone Bevy 0.20.1 viewport shell, not yet the complete editor.
It currently renders a rotating sample primitive. It is not connected to the
Rust API, and it does not yet load GLB files, pick semantic anchors, paint
weights, display review annotations, or edit Motion tracks.

Run from this directory:

```powershell
cargo run
```

The desktop shell is deliberately separated from the backend workspace so the
headless Rust API and domain tests do not require Bevy's full rendering stack.
