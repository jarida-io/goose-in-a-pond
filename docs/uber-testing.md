# Testing Uber rides against your own Uber app

How to try ride booking end to end with an Uber developer app you created, against Uber's sandbox,
so no car is dispatched. The pond's side is described in [api.md § Rides and Uber
accounts](api.md#rides-and-uber-accounts); the relay's design is in
[architecture/pondcredentials.md](architecture/pondcredentials.md).

## How the pieces fit

- The pond talks to Uber's Riders API (v1.2) with each member's own token
  (`crates/pond-adapters-uber`). It asks for the scopes `request`, `profile` and `offline_access`.
- An Uber app's client secret cannot ship inside a pond. Sign-in goes through the credentials relay
  (`services/pondcredentials`): the pond asks it for the client ID, sends the browser to Uber, and
  hands the returned code to the relay, which adds the secret and exchanges it. The tokens are kept
  only in the pond's encrypted secret store, under keys starting `UBER_`.
- So testing with your own app means running your own relay with your app's ID and secret, and
  pointing the pond at it.

## 1. Create the Uber app

1. In the Uber developer dashboard choose **Create Application**. Pick the suite that offers ride
   requests (the Riders API); if none is listed for your account, pick **Others** and ask Uber for
   Riders API access. Choose **Testing**.
2. Copy the **Client ID** and **Client Secret**. Keep the secret out of the repository and out of
   chat.
3. Add this redirect URI exactly: `http://127.0.0.1:4000/api/v1/oauth/callback`. The pond always
   sends the `127.0.0.1` loopback form on the port the dashboard reaches it on
   (`crates/pond-api/src/oauth_callback.rs`). If you open the dashboard on another port, register
   that port as well.
4. Enable the scopes `profile` and `request` (and `offline_access` if listed). `request` is
   privileged: production needs Uber's approval, the sandbox does not.
5. Add your own Uber account as a developer or test user of the app, if the dashboard asks.

## 2. Run the relay locally

The relay also signs Apple Music tokens and refuses to start without Apple settings. For an
Uber-only test, a throwaway P-256 key is enough; Apple Music will not work with it.

```bash
mkdir -p ~/.giap-uber-test && cd ~/.giap-uber-test
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out apple-throwaway.p8
printf '%s' 'YOUR_UBER_CLIENT_SECRET' > uber_client_secret && chmod 600 uber_client_secret
```

```bash
cd services/pondcredentials
APPLE_TEAM_ID=TESTTEAM00 APPLE_KEY_ID=TESTKEY000 \
APPLE_PRIVATE_KEY_FILE=$HOME/.giap-uber-test/apple-throwaway.p8 \
UBER_CLIENT_ID=YOUR_UBER_CLIENT_ID \
UBER_CLIENT_SECRET_FILE=$HOME/.giap-uber-test/uber_client_secret \
UBER_TOKEN_URL=https://sandbox-login.uber.com/oauth/v2/token \
PONDCREDENTIALS_BIND=127.0.0.1:8088 \
cargo run --release
```

`GET http://127.0.0.1:8088/v1/uber/client` should answer with your client ID; a `503` means the Uber
variables were not picked up.

A test app is known only to Uber's sandbox sign-in server, `sandbox-login.uber.com`, so the relay
exchanges codes there (`UBER_TOKEN_URL`). Production's server, the default, refuses a test app with
`unauthorized_client` ("the current application environment is mismatched"). Leave `UBER_TOKEN_URL`
unset for a production app.

## 3. Start the pond against the relay and the sandbox

```bash
POND_CREDENTIALS_URL=http://127.0.0.1:8088 \
GIAP_UBER_API_BASE=https://sandbox-api.uber.com \
cargo run -p pond-server -- serve
```

Both variables accept plain `http` only for this machine. With the sandbox API, the pond also sends
members to the sandbox's sign-in page (`UberConfig::authorize_url`). Leave `GIAP_UBER_API_BASE` unset to use
production, which dispatches real cars. `GIAP_UBER_PRODUCT_ID` pins a product; unset lets Uber choose.

## 4. Connect a member

1. Settings: switch on **Travel** (`ext_travel_enabled`).
2. Context → Sources → **Uber**: press **Connect Uber** beside the household member. Sign in to Uber in the
   browser that opens and approve the scopes. The panel shows the member as connected
   (`GET /api/v1/uber/accounts`).
3. If sign-in fails, the relay's log and the pond's `uber_relay_failed` error say why. Uber's
   "Authorisation error: Invalid client" page means the sign-in went to a server that does not know
   the app: a test app with the pond on the production API, or the reverse. A redirect
   mismatch means the URI in step 1.3 does not match the dashboard's port.

## 5. Book a sandbox ride

1. Use a phone whose pairing names the member you connected (rides are refused, `403`, for a pairing
   without a member).
2. Ask Goose for a ride, for example "get me an Uber to the airport". It shows an upfront fare;
   confirm it.
3. Follow it with `GET /api/v1/rides/{id}`. In the sandbox a ride stays `processing` until you move
   it on with Uber's sandbox endpoint, `PUT https://sandbox-api.uber.com/v1.2/sandbox/requests/{request_id}`
   with `{"status": "accepted"}` (then `arriving`, `in_progress`, `completed`), using the member's
   access token.
4. Cancel from Goose to check cancellation.

## Cleaning up

- `DELETE /api/v1/uber/accounts/{profile_id}`, or Disconnect in the panel, forgets the member's tokens.
- Delete `~/.giap-uber-test` when you are done, and rotate the client secret if it was ever pasted
  anywhere else.
