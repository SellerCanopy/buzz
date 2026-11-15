-- A card's artifacts: documents a stage writes for the next one, and pointers to files in the
-- Buzz media store. `handoff` says whether later stages get the artifact by default.
CREATE TABLE hive.artifacts (
    community_id uuid    NOT NULL,
    card_id      uuid    NOT NULL,
    name         text    NOT NULL,
    kind         text    NOT NULL CHECK (kind IN ('text','file')),
    handoff      boolean NOT NULL DEFAULT true,
    PRIMARY KEY (community_id, card_id, name),
    FOREIGN KEY (community_id, card_id) REFERENCES hive.cards (community_id, id) ON DELETE CASCADE
);

-- Each save appends a version. A text version holds `body`. A file version holds the hash,
-- size and type of a blob in the media store, and `media_ref` names where to fetch it.
-- `visit` is the stage's visit that saved it, so a stage can tell what it saved this time.
CREATE TABLE hive.artifact_versions (
    community_id uuid        NOT NULL,
    card_id      uuid        NOT NULL,
    name         text        NOT NULL,
    version      int         NOT NULL CHECK (version >= 1),
    body         text,
    sha256       text,
    size         bigint,
    media_type   text,
    media_ref    text,
    stage        text        NOT NULL,
    visit        int         NOT NULL DEFAULT 0 CHECK (visit >= 0),
    created_by   text        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, card_id, name, version),
    FOREIGN KEY (community_id, card_id, name)
        REFERENCES hive.artifacts (community_id, card_id, name) ON DELETE CASCADE,
    CHECK ((body IS NOT NULL) <> (sha256 IS NOT NULL))
);
