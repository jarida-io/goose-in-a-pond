-- voice_wake_word: 'goose' -> 'hey goose'.
--
-- "goose" alone is said in passing, and the ear's wake model knows only "hey goose". Guarded on
-- the stored value and on `is_user_set`, per migration 0035, and on no calibration: learned
-- pronunciations were heard for the old phrase, so a household that calibrated keeps it. The
-- registry entry is `DEFAULT_ADOPTIONS` in `pond-core/src/user_data/domain/settings.rs`.
UPDATE settings
   SET value = 'hey goose', updated_at = datetime('now')
 WHERE key = 'voice_wake_word'
   AND value = 'goose'
   AND is_user_set = 0
   AND NOT EXISTS (
       SELECT 1 FROM settings AS learned
        WHERE learned.key = 'voice_wake_word_transcriptions'
          AND learned.value NOT IN ('', '[]')
   );
