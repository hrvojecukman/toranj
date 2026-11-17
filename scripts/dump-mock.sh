#!/bin/sh
# Dumps the live state (tmux tabs, Claude sessions, projects, terminal screens)
# into src/dev/mock-data.json so the UI can be rendered in a normal browser
# with ?mock and screenshotted without macOS screen-recording permission.
set -e
cd "$(dirname "$0")/.."
out=$(cd src-tauri && cargo test --lib -- --ignored --nocapture 2>/dev/null)
snap=$(printf '%s\n' "$out" | sed -n 's/^SNAPSHOT_JSON=//p')
projects=$(printf '%s\n' "$out" | sed -n 's/^PROJECTS_JSON=//p')
resources=$(printf '%s\n' "$out" | sed -n 's/^RESOURCES_JSON=//p')
SNAP="$snap" PROJECTS="$projects" RESOURCES="$resources" python3 -I - <<'PY'
import json, os, subprocess
snap = json.loads(os.environ["SNAP"]); projects = json.loads(os.environ["PROJECTS"])
screens = {}
for t in snap["tabs"]:
    r = subprocess.run(["tmux", "-L", "toranj", "capture-pane", "-e", "-p", "-t", t["name"]], capture_output=True, text=True)
    screens[t["name"]] = r.stdout
res = json.loads(os.environ["RESOURCES"] or "null")
json.dump({"snapshot": snap, "projects": projects, "screens": screens, "resources": res}, open("src/dev/mock-data.json", "w"))
print(f"{len(snap['tabs'])} tabs, {len(snap['external'])} external, {len(projects)} projects")
PY
