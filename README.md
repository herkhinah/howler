# howler

Receives Prometheus Alertmanager webhook events, renders them via jira2 templates and forwards them to chosen matrix rooms.

Features:

- batches alerts for the same target room which arrive in short succession into a single message
- backup bots
- checks if messages federate to at least one other servers
- forwards different webhook url paths into different matrix room
- per room configurable jinja2 templates for rendering alerts(configured via state events)

## Usage

### Configuration

Modify the [sample config](config.sample.yaml) to setup the server. To set up a room set the described [state events](#custom-state-events) and invite the bots into the room.

### custom state events

#### com.famedly.howler_template

Used to configure room specific jira2 templates.

```json
{
  "type": "com.famedly.howler_template",
  "content": {
    "html": "jira2 template for `content.formatted_body`",
    "plain": "jira2 template for `content.body`",
  },
  "state_key": "",
  ...
}
```

#### com.famedly.howler_webhook_access_token

Used to configure the alertmanager webhook receiver token for the room.

```json
{
  "type": "com.famedly.howler_webhook_access_token",
  "content": {
    "token": "access token for room",
  },
  "state_key": "",
  ...
}
```

The webhook url path for the room is `/room_id/access_token`.

### Notes

For the federation confirmation feature to properly work it's important that all bots are hosted on different servers.

## License

[AGPL-3.0-only](https://choosealicense.com/licenses/agpl-3.0/)
