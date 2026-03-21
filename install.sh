#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="api-proxy"

install_linux() {
    local service_dir="$HOME/.config/systemd/user"
    local override_dir="$service_dir/$BIN_NAME.service.d"
    mkdir -p "$service_dir" "$override_dir"
    cp api-proxy.service "$service_dir/$BIN_NAME.service"

    # Capture the user's current PATH so the service can find claude, gh, etc.
    cat > "$override_dir/path.conf" <<EOF
[Service]
Environment="PATH=$PATH"
EOF

    systemctl --user daemon-reload
    systemctl --user enable --now "$BIN_NAME"
    echo "Installed systemd user service. Check status with: systemctl --user status $BIN_NAME"
}

install_macos() {
    local plist_name="com.github.$BIN_NAME"
    local plist_dir="$HOME/Library/LaunchAgents"
    local plist_path="$plist_dir/$plist_name.plist"
    local bin_path="$INSTALL_DIR/$BIN_NAME"

    mkdir -p "$plist_dir"
    cat > "$plist_path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$plist_name</string>
    <key>ProgramArguments</key>
    <array>
        <string>$bin_path</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>$HOME/Library/Logs/$BIN_NAME.log</string>
    <key>StandardErrorPath</key>
    <string>$HOME/Library/Logs/$BIN_NAME.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/usr/local/bin:/usr/bin:/bin:$HOME/.cargo/bin</string>
    </dict>
</dict>
</plist>
EOF

    launchctl bootout "gui/$(id -u)" "$plist_path" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$plist_path"
    echo "Installed launchd service. Check status with: launchctl print gui/$(id -u)/$plist_name"
}

# Build and install binary
echo "Building $BIN_NAME (release)..."
cargo install --path . --force
echo "Installed to $(which $BIN_NAME)"

# Install and start service
OS="$(uname -s)"
case "$OS" in
    Linux)
        install_linux
        ;;
    Darwin)
        install_macos
        ;;
    *)
        echo "Unknown OS: $OS. Binary installed, but you'll need to set up the service manually."
        exit 0
        ;;
esac
