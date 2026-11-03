-- A member's wake template from each enrolment take: the phrase's fbank frames, and the frames
-- after it, packed little-endian f32 (80 per frame). Features only, never audio. They go with the
-- take, so forgetting a member's takes, or the member, forgets them; forgetting a member's voice
-- clears them on its own.
ALTER TABLE voice_wake_takes ADD COLUMN template BLOB;
ALTER TABLE voice_wake_takes ADD COLUMN template_rest BLOB;
