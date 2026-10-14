-- Wissensdatenbank / Help Center.

CREATE TABLE kb_categories (
    id         BIGSERIAL PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    position   INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE kb_articles (
    id           BIGSERIAL PRIMARY KEY,
    category_id  BIGINT REFERENCES kb_categories(id) ON DELETE SET NULL,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL DEFAULT '',
    status       TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    author_id    BIGINT REFERENCES users(id) ON DELETE SET NULL,
    views        INT NOT NULL DEFAULT 0,
    helpful_yes  INT NOT NULL DEFAULT 0,
    helpful_no   INT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    search       TSVECTOR GENERATED ALWAYS AS (
                     setweight(to_tsvector('german', title), 'A') || setweight(to_tsvector('german', body), 'B')
                 ) STORED
);
CREATE INDEX kb_articles_search_idx ON kb_articles USING GIN (search);
