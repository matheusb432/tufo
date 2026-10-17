# Command line

`tufo` can do everything the app can. `tufo --help` lists the commands, and each command's
`--help` shows its arguments, JSON fields, and examples.

Most commands need the server running:

```sh
tufo server start
```

Changes take one JSON object, and fields you leave out stay as they are. Add `--json` for
machine-readable output. Times are in milliseconds and volumes go from 0 to 1. Commands exit with
0 on success, 1 on failure, and 2 for bad arguments.
