# Rides and maps

How the pond helps a member get somewhere: directions in a maps app, an Uber link with the trip
filled in, and an Uber ride booked through the assistant. All of it is behind `ext_travel_enabled`,
which ships off.

**No ride is requested until the member confirms it on their own paired phone.** The assistant can
offer a ride; only the member's phone, after showing the fare, can book one.

## What the ride companies allow

| | Book and track from another app | Open the app with a trip filled in |
|---|---|---|
| **Uber** | The Ride Requests API. New apps need approval through Uber business development ([Uber developer docs](https://developer.uber.com/docs/riders/introduction)). | Yes. The documented universal link `https://m.uber.com/ul/?action=setPickup` takes pickup and drop-off. A drop-off needs a latitude and longitude ([deep links](https://developer.uber.com/docs/riders/ride-requests/tutorials/deep-links/introduction)). |
| **Bolt** | No public or private API. Booking integrations are for strategic partners only ([Bolt support](https://bolt.eu/en/support/articles/360017256060/)). | No documented link format. |

Reverse-engineered APIs and automating the apps' screens are ruled out: they break both companies'
terms, risk the member's account, and would put the pond between the member and their payment
method.

## The travel extension

`giap-travel` (`TRAVEL_EXTENSION` in `pond-core`'s `tool_group.rs`; code in
`crates/pond-mcp-server/src/travel.rs`) is registered in `giap_registration.rs` when
`ext_travel_enabled` is on at startup.

| Tool | Does | Leaves the pond |
|---|---|---|
| `get_directions_link` | Google Maps and Apple Maps directions to a place, for driving, walking, transit or cycling. With no origin, the maps app starts from the phone's location. | Nothing. The maps apps resolve free text themselves, so the links are built locally. |
| `get_ride_link` | An Uber link with pickup and drop-off filled in. With no pickup, Uber uses the phone's location (`pickup=my_location`). Asking for Bolt returns an error result: Bolt has no such link. | Place names, to the Open-Meteo geocoder for coordinates. |
| `book_ride` | Sends a ride offer to the speaker's own phone, which gets the fare and books only when they confirm. | The destination name, to the geocoder. |

The direct dispatcher (`POST /api/v1/tools/invoke`) does not route it: it reads no settings, and
the push and `book_ride` need an engine session that route does not carry (`DISPATCHER_EXCLUSIONS`
in `registration_matches_the_catalog.rs`). A guest and a subagent are never offered it
(`groups_denied_to_guests`, `groups_denied_to_subagents`).

### Why it ships off

- Three more tool schemas in every prompt, under the default `tool_selection_mode` (`all`).
- Place names are personal and go to the geocoder, though not who asked. `open-meteo.com` is a known
  public host, so `allowlist` mode lets it through and `offline` refuses it. Under `offline`,
  `get_ride_link` still returns an Uber link with the drop-off left empty, and `book_ride` sends
  nothing.
- A booking spends a member's money.

### Matching a place name

Open-Meteo matches towns, neighbourhoods and well-known places, not street addresses, and ranks
namesakes worldwide: its first "Westlands" is in Jamaica.

- The pond asks for ten matches and takes the one nearest home, the location in Settings
  (`weather_latitude`, `weather_longitude`). With none within 50 km the result says "no place
  matched within 50 km of home": `get_ride_link` leaves the drop-off empty and `book_ride` sends no
  offer.
- It searches only the home's country once it knows it. It learns it once per home location, by
  matching the pond's own place name (`weather_location_name`, else its time zone's city) near its
  coordinates: one more lookup, of that name.
- With no home location set it takes the best match anywhere, and the result says so.
- The result always names the place matched ("Westlands, Kenya (matched from "Westlands")"), and
  Uber shows the pin before the member confirms.

### Results state facts

The `get_ride_link` result ends with a line saying nothing has been booked, the `book_ride` result
says nothing is booked until the member confirms on their phone, and both tools' descriptions say
never to claim a ride is booked. As in `giap-weather`, results state facts and never tell the model what to say,
which a test in `travel.rs` enforces.

## Sending to the member's phone

Links and ride offers also go to the phone of the person who asked, as a notification.

1. **Who is speaking.** `pond-mcp-server` keeps one speaker authority (`init_speaker_authority`, a
   `RepoDraftAuthority`), shared with `giap-context`. Each tool call reads the engine session from
   its `_meta`. A speaker identified as one member (`Owner`) is that member. An unidentified speaker
   in a one-member household (`Household`) is its only member (`DraftAuthority::sole_member`). A
   household of several, a guest, an unresolved session and a call without one get the link in the
   reply only, and the reply says why.
2. **Which phones are theirs.** The `MemberNotifier` port (`mcp/ports/notification.rs`),
   implemented by `BroadcastNotificationSender` over `send_to_profile`, delivers to the member's
   attributed devices through `DeviceAttribution::devices_for_profile` and never falls back to a
   broadcast. It answers whether it reached their phones, they have none, or delivery failed, and
   the reply says which.
3. **Where it runs.** `serve` and `pond-server chat` (the desktop's voice screen runs it as a child
   process) both install the speaker authority, the member notifier and the members' Uber accounts.
   The travel server reads them each time a tool runs, so a server goose spawned before startup
   finished still uses them. From the voice child a notification is queued in `pond_system.db`, and
   `serve` hands it over when the phone opens its notification stream
   (`/api/v1/notifications/stream`), which the push relay prompts when the pond has an FCM key.
4. **GOTG opens it.** For a link, `Notification.data` carries:

   ```json
   {
     "action": "open_url",
     "kind": "ride",
     "url": "https://m.uber.com/ul/?action=setPickup&pickup=my_location&dropoff[latitude]=...",
     "label": "Open Uber"
   }
   ```

   `kind` is `ride` or `directions`; for directions the pond sends the Google Maps link, which opens
   Google Maps where it is installed and the browser otherwise. The category is `info`. GOTG
   (`services/notification-link.ts`, in its own repository) opens only `https` links on
   `m.uber.com`, `maps.apple.com` and `www.google.com/maps`, with the system handler.

   A ride offer is category `action_required`, titled "Ride to {place}?", with
   `{"action": "ride_offer", "provider": "uber", "dropoff": {"name", "latitude", "longitude"},
   "label": "Get a fare"}`.

**Bolt.** GOTG could open the Bolt app (`ee.mtakso.client` on Android, App Store id `675033630` on
iOS) with the destination in the notification body. That isn't built: the pond pushes nothing for
Bolt.

## Booking an Uber

Code: `pond-core/src/rides` (booking and tracking), `pond-adapters-uber` (Uber's API and the
members' sign-ins), `pond-api/src/rides.rs` (the phone's routes), `pond-server/src/ride_booking.rs`
(startup).

- **Accounts.** Each member connects their own Uber account from the desktop's Accounts screen
  (`/api/v1/uber/accounts`), through Jarida's credentials service (`POND_CREDENTIALS_URL`), which
  holds the Uber app's client secret. The member's tokens are kept in the pond's secret store and
  renewed there.
- **The offer.** `book_ride` checks that the speaker is one member who has connected Uber and
  matches the destination as above, then sends the `ride_offer` above. It books nothing.
- **The fare and the confirmation.** The phone asks `POST /api/v1/rides/quote` for an upfront fare
  from where it is (refused for a drop-off more than 150 km from the pickup), then confirms or
  declines it (`docs/api.md`, *Rides*). The member is whoever the phone's pairing names, and another
  member's ride reads as missing. A fare is confirmed at most once, and a request is never retried.
- **A lost answer.** A clear refusal from Uber fails the ride, and so does `current_trip_exists`:
  the pond sends each request once, so the trip Uber means is one the member booked some other way,
  and it is not taken as this ride. A timeout, a dropped connection or a server error may mean Uber
  booked it, so the pond asks Uber for the member's trip under way (`/v1.2/requests/current`) and
  takes it as the ride. Without one the
  ride's outcome is unknown: the phone's confirm answers 202 and says to check the Uber app, and the
  tracker keeps asking.
- **Tracking.** Every `GIAP_RIDE_POLL_SECS` (default 15, at least 5) the pond reads each ride under
  way and sends a `ride_update` on each change: driver assigned, arriving, trip started, arrived, no
  drivers, cancelled. An update that fails to send is sent again on the next pass. Its notification
  id is a UUID that names neither the ride nor its status, because the push relay's FCM message
  carries it. After 40 reads in a row fail, the pond stops reading the ride and tells the member the
  ride app shows where it stands.
- **Memory.** Each ride is kept in the `rides` table of `pond_system.db` (migration 0060,
  `pond-infra/src/sqlite_ride_store.rs`), written on every change: the ride, the last status its
  member was told, and its failed reads. At startup the pond takes them all back before the phone's
  routes can be asked, so a fare quoted before a restart can still be confirmed, an update is not
  sent twice, and the 40-read limit keeps counting. A ride that was being requested when the pond
  stopped is never requested again: it comes back as an outcome unknown, settled by the member's
  trip under way like any lost answer. The pond still takes over each connected member's trip under
  way, for one booked outside the pond. A ride that is over is forgotten, and its row deleted, a
  day after it was quoted or taken over; a removed member's rides go with them (`ON DELETE
  CASCADE`). A store that fails is logged and does not fail the member: the ride carries on in
  memory.
- **When it runs.** Only while `ext_travel_enabled` is on. Off at startup, the ride routes answer
  503 and nothing is tracked; switched off later, no new fare is quoted or confirmed, though a ride
  already quoted or booked can still be read, declined or cancelled. Booking also needs the secret
  store and the credentials service, and `GIAP_UBER_API_BASE` (default production;
  `https://sandbox-api.uber.com` simulates rides) must be https, or http to this machine for tests.
- **Network.** Uber's hosts are `Sensitive` in the egress classification, so `allowlist` and
  `offline` refuse them. Uber calls are filed under `giap-rides` and sign-in calls under
  `giap-credentials`, never under a chat session.
- **Uber's approval.** The `request` scope is privileged: production use needs Uber's Full Access
  approval. The sandbox works without it.

## Not planned

- Background location from GOTG. A ride is booked from where the phone is, which the phone already
  knows. GIAP has no need for a location history.
- Fare comparison across apps. Bolt publishes no fares to compare.
