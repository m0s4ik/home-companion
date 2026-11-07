<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Home Assistant WebSocket fixtures

One message per line, as Home Assistant 2026.x sends them on `/api/websocket` after
`subscribe_entities` (id 1, `entity_ids: ["weather.home"]`) and `weather/subscribe_forecast` (id 2,
`forecast_type: "daily"`). Values are made up; the shapes follow a Met.no weather entity.
`weather-unavailable.jsonl` is the entity while its integration has no data.
