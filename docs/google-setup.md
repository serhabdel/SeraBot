# Google setup (admin)

SeraBot uses one Google OAuth client for the whole server. Each person then
connects their own Google account from Connectors; one consent covers the
products they enable (Drive, Sheets, Calendar, Gmail) and more can be added
later. Tokens are stored per user and never shared.

## 1. Create the OAuth client

1. Open <https://console.cloud.google.com/> and create or pick a project.
2. APIs & Services, Library: enable **Google Drive API**, **Google Sheets API**,
   **Google Calendar API** and **Gmail API**.
3. APIs & Services, OAuth consent screen:
   - User type **External**, fill in the app name and support email.
   - Add these scopes: `openid`, `email`, `.../auth/drive.readonly`,
     `.../auth/spreadsheets.readonly`, `.../auth/calendar.readonly`,
     `.../auth/gmail.readonly`.
   - While the app is in **Testing**, add every person who will connect as a
     **test user** (up to 100). Testing-mode refresh tokens expire after 7 days;
     publish the app (or move it to Production) for long-lived connections.
     Gmail and Drive scopes are "restricted", so Google may ask for
     verification before Production; Testing is fine for a small circle.
4. APIs & Services, Credentials, **Create credentials**, **OAuth client ID**:
   - Application type **Web application**.
   - Authorized redirect URI (exactly, no trailing slash):
     `https://<your-server-base>/v1/connectors/google/callback`
     where `<your-server-base>` is the server's public https address
     (`auth.base_url`).
5. Copy the **Client ID** and **Client secret**.

## 2. Save it in SeraBot

Open the desktop app as an admin, Connectors, **Set up Google**, paste the
client ID and secret, Save. They go to the server's secret store
(`google_oauth_client_id`, `google_oauth_client_secret`) through
`PUT /v1/admin/connectors/google_drive` and are never returned. No restart is
needed. The Google cards then change from "Not set up yet" to Connect.

Equivalent CLI: `serabot secrets set google_oauth_client_id` and
`serabot secrets set google_oauth_client_secret`.

## 3. Each person connects

Connectors, pick a Google product, **Connect**, approve in the browser. The
card then reads "Connected as <email>". Enabling another Google product asks
for consent again with the scopes already granted plus the new one.

## Troubleshooting

- `redirect_uri_mismatch`: the redirect URI in step 1.4 differs from
  `<base>/v1/connectors/google/callback`.
- `access_denied` / "app not verified": the person is not a test user.
- Reads fail after about a week: the app is still in Testing; publish it.
