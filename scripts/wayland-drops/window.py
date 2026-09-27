"""Drives Tufo's window through the Slint MCP server compiled into a debug build.

    python3 window.py <mcp port> open-playlists
"""

import json
import sys
import urllib.request

WINDOW = {"generation": "1", "index": "1"}


def call(port, tool, arguments):
    body = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}
    ).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/mcp",
        body,
        {"content-type": "application/json", "accept": "application/json, text/event-stream"},
    )
    raw = urllib.request.urlopen(request, timeout=10).read().decode()
    if "data:" in raw[:20]:
        raw = [line[5:] for line in raw.splitlines() if line.startswith("data:")][-1]
    return json.loads(json.loads(raw)["result"]["content"][0]["text"])


def buttons(port):
    root = call(port, "get_window_properties", {"windowHandle": WINDOW})["rootElementHandle"]
    found = call(
        port,
        "query_element_descendants",
        {
            "elementHandle": root,
            "findAll": True,
            "queryStack": [{"matchDescendants": True}, {"matchElementAccessibleRole": "Button"}],
        },
    )
    for handle in found.get("elementHandles", []):
        yield handle, call(port, "get_element_properties", {"elementHandle": handle})


def open_playlists(port):
    see_all = sorted(
        (properties["absolutePosition"]["y"], index, handle)
        for index, (handle, properties) in enumerate(buttons(port))
        if properties.get("accessibleLabel") == "See All"
    )
    if not see_all:
        raise SystemExit("the home page shows no See All button")
    call(port, "click_element", {"elementHandle": see_all[0][2], "action": "SingleClick", "button": "Left"})


if __name__ == "__main__":
    port, action = int(sys.argv[1]), sys.argv[2]
    {"open-playlists": open_playlists}[action](port)
