#!/usr/bin/env bash
# scripts/service.sh
#
# Manage the mibee-rec user-mode systemd service on a remote device.
#
# Usage:
#   ./scripts/service.sh <host> <command>
#
# Commands:
#   install   Install/update the ~/.config/systemd/user/mibee-rec.service unit
#   start     Install (if needed) + start (or restart) the service
#   stop      Stop the service
#   status    Show service status
#   logs      Follow journalctl logs (Ctrl-C to exit)
#   restart   Stop + start
#   disable   Stop + disable linger + remove unit file
#
# Hosts (from ~/.ssh/config):
#   device-1   any Linux host with user services (Device 1)
#   device-2   any Linux host with user services (Device 2)

set -euo pipefail

HOST="${1:-}"
CMD="${2:-}"

if [ -z "$HOST" ] || [ -z "$CMD" ]; then
    echo "Usage: $0 <ssh-host-alias> <install|start|stop|status|logs|restart|disable>"
    echo ""
    echo "Hosts: read from ~/.ssh/config aliases, e.g. device-1, device-2"
    exit 1
fi

# The systemd user unit, adapted from the repo's system-wide mibee-rec.service.
# Runs as the SSH login user, reads config.local.toml, working dir ~/mibee-rec.
# NOTE: SupplementaryGroups= is NOT supported in user-mode systemd (it requires
# root to change group credentials → status=216/GROUP). The user must already be
# in the video + audio groups (set via `usermod -aG video,audio $USER`).
read -r -d '' UNIT_BODY <<'EOF' || true
[Unit]
Description=MiBee Rec — local surveillance agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=%h/mibee-rec
ExecStart=%h/mibee-rec/mibee-rec --config %h/mibee-rec/config.local.toml
Restart=on-failure
RestartSec=5

# Minimal sandboxing. Note: ProtectHome=/ProtectSystem= are omitted because
# the service must write its SQLite DB (mibee_rec.db) + recordings into the
# user's home directory, and must access /dev/video* + audio devices.
# NoNewPrivileges= is safe to keep.
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF

install_unit() {
    echo "→ Installing systemd user unit on $HOST..."
    # Write the unit file via a heredoc over SSH.
    ssh "$HOST" "mkdir -p ~/.config/systemd/user && cat > ~/.config/systemd/user/mibee-rec.service <<'UNIT_EOF'
$UNIT_BODY
UNIT_EOF
"

    # Enable linger so the user service starts at boot (and survives logout).
    # This requires `loginctl enable-linger` which may need polkit or sudo.
    # We try without sudo first; if it fails we print a clear message.
    if ! ssh "$HOST" "loginctl enable-linger \$USER 2>/dev/null"; then
        echo "  NOTE: 'loginctl enable-linger' failed (needs sudo on some distros)." >&2
        echo "        On $HOST run: sudo loginctl enable-linger \$USER" >&2
        echo "        This is only needed for boot-time auto-start." >&2
    fi

    ssh "$HOST" "systemctl --user daemon-reload"
    echo "  Unit installed and daemon reloaded."
}

case "$CMD" in
    install)
        install_unit
        ;;
    start)
        install_unit
        echo "→ Starting service on $HOST..."
        ssh "$HOST" "systemctl --user restart mibee-rec && systemctl --user status mibee-rec --no-pager -l | head -20"
        echo ""
        echo "✓ Service started. Use '$0 $HOST logs' to follow."
        ;;
    stop)
        echo "→ Stopping service on $HOST..."
        ssh "$HOST" "systemctl --user stop mibee-rec 2>/dev/null || true"
        echo "✓ Stopped."
        ;;
    restart)
        echo "→ Restarting service on $HOST..."
        ssh "$HOST" "systemctl --user restart mibee-rec && systemctl --user status mibee-rec --no-pager -l | head -20"
        ;;
    status)
        ssh "$HOST" "systemctl --user status mibee-rec --no-pager -l" || true
        ;;
    logs)
        echo "→ Following journalctl on $HOST (Ctrl-C to exit)..."
        ssh "$HOST" "journalctl --user -u mibee-rec -f"
        ;;
    disable)
        echo "→ Disabling + removing service on $HOST..."
        ssh "$HOST" "systemctl --user stop mibee-rec 2>/dev/null || true; systemctl --user disable mibee-rec 2>/dev/null || true; rm -f ~/.config/systemd/user/mibee-rec.service; systemctl --user daemon-reload"
        echo "✓ Service disabled. (loginctl linger left as-is.)"
        ;;
    *)
        echo "ERROR: unknown command '$CMD'" >&2
        echo "Commands: install | start | stop | status | logs | restart | disable" >&2
        exit 1
        ;;
esac
