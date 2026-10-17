# Outputs and Discord

Audio plays on one output at a time: this computer or a Discord voice channel. Pick it from the
output picker in the sidebar.

## Set up a bot

1. Create an application in the [Discord Developer Portal](https://discord.com/developers/applications)
   and copy its bot token.
2. Invite the bot to your server with the `bot` scope and the **View Channels**, **Connect**, and
   **Speak** permissions.
3. Paste the token in Settings.
4. Pick a voice channel in the output picker.

If a channel has its own permissions, the bot needs them there too. The token is stored in
`discord.toml` in your [config folder](troubleshooting.md#data-locations); keep it private and
reset it in the portal if it leaks.

## This computer

On Linux, audio goes through PipeWire, or ALSA without it. Set `PIPEWIRE_NODE` before starting the
server to use a specific node.
