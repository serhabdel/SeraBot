# Job search with JobFinder

JobFinder (the `jobfinder` bot) can watch Moroccan job sites for you, tell you about new postings that fit, and keep track of your applications. Everything here is yours alone: other members never see your profile, sources, matches or applications.

## First chat: your profile

The first time you talk to JobFinder it asks a few short questions, one at a time, in your language:

- what roles or fields you want, in which cities (or remote)
- public, private or internship jobs
- your languages and level
- words to include or avoid, and employers to exclude
- how often to check: daily, twice a day or weekly (or off)
- optional: your own OneDrive Excel tracker, your OneDrive folder with CVs, letters and diplomas, and whether deadlines of public concours go into your tasks

You can answer "skip" to any of them and ask JobFinder to change the profile later. The "Job search" card on the Usage page shows a summary.

## Watching sites

The built-in sources are emploi-public.ma (concours), alwadifa-maroc.com, anapec.org, rekrute.com, emploi.ma, dreamjob.ma, stagiaire.ma and marocannonces.com (jobs). You can add up to 10 listing pages of your own and switch **auto-watch** on or off for each source.

- SeraBot reads one page per source per check, a few seconds apart.
- If a site's `robots.txt` asks for no automatic visits, that source is skipped on the schedule. It is read only when you ask ("check now").
- Postings are matched against your profile (roles and keywords, cities, sector, exclusions). Each posting is announced once.
- New matches arrive as one message on your alert channel (Telegram or the WhatsApp chat you chose under alerts): at most 8 lines, each with title, employer, city, deadline and link. If you have no alert channel, ask JobFinder or press "Check now" on the card.
- If you turned on "deadlines to tasks", the deadline of a matching public concours becomes a task.

Listing pages change. If a site shows nothing, JobFinder says so; it never makes postings up. The built-in addresses can be replaced by adding a corrected one as your own source.

## Applications

Ask JobFinder to save a job, mark it applied, set an interview date, add notes, or link the CV and letter it prepared. Statuses go saved, applied, interview, offer; rejected can come at any step and closes the application. The card shows the count per status.

If you gave a OneDrive tracker, JobFinder offers to update it: it downloads your file, edits the row and uploads it back, and you approve the upload.

## For the owner

- The watch runs once a minute from the ops loop, per member, at their chosen frequency.
- Fetching uses the same SSRF-guarded fetcher as the watcher (public addresses only, size and time limits).
- API (member-scoped): `GET/PUT /v1/jobs/profile`, `GET /v1/jobs/sources`, `PUT /v1/jobs/sources/{key}`, `GET /v1/jobs/applications`, `GET /v1/jobs/overview`, `POST /v1/jobs/watch`.
- The built-in listing addresses were not checked against the live sites; verify them on a real host.
