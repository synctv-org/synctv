-- P2 Migration: Add CHECK constraint for rooms.description length (DB-11)
--
-- The rooms.description column is TEXT with a documented max of 500 characters
-- (see COMMENT ON COLUMN rooms.description), but no database-level constraint
-- enforces this. Application-level validation exists but defense-in-depth
-- requires a DB constraint to prevent oversized descriptions from bulk imports,
-- direct SQL, or future API bugs.

ALTER TABLE rooms
    ADD CONSTRAINT rooms_description_length_check
    CHECK (length(description) <= 500);

COMMENT ON CONSTRAINT rooms_description_length_check ON rooms IS
'Room description must not exceed 500 characters';
