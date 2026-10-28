-- What the person is building and which chat app they'd join a community on.
ALTER TABLE waitlist ADD COLUMN business TEXT;
ALTER TABLE waitlist ADD COLUMN community TEXT;
