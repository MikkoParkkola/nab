# Privacy

nab can read cookies stored by the local browser so a fetch can reuse a session you already have. A named session stores its cookies on this machine. Cookie values are not sent to the author. Fetching stays on this machine. Searches and fetches go to the URL you asked for.

This plugin's launcher sends at most one POST per day to `https://telemetry.revaluator.ai/v1/heartbeat`. It then starts the published nab-mcp program this folder downloads. That program sends the same POST only when its build includes this client. The published build pinned by this folder does not. The launcher and a later build share one install id and one daily stamp under `~/.nab/telemetry`, so a day produces one attempt.

The JSON fields are only `project` (`nab`), `event` (`heartbeat`), `version`, `runtime` (operating system, architecture, and the runtime version), and `install_id`. The install id is 16 random bytes, stored on this machine. The body has no hostname, no username, no cookie, and no URL you fetched. The request times out after 3 seconds, and a failed send is ignored.

Cloudflare terminates that connection, so Cloudflare can see the caller IP. The receiver stores the connection's city name and country code. Coordinates and the IP are not written into the stored point. Records stay in Cloudflare Analytics Engine for three months.

Development builds, tests, and CI skip this POST. Set `NAB_NO_TELEMETRY`, `NO_TELEMETRY`, or `DO_NOT_TRACK` to a value other than `0` or `false` to turn it off. `NAB_TELEMETRY_ENDPOINT` replaces the URL. An empty value leaves the default.

Support: Mikko Parkkola via GitHub issues at https://github.com/MikkoParkkola/nab/issues
