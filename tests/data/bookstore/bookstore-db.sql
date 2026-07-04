-- Schema for a small online bookstore, used by the injection integration tests.
-- Written to be idempotent so it can be (re)applied before every test run.

CREATE TABLE IF NOT EXISTS public.author (
    author_id integer PRIMARY KEY,
    name      text NOT NULL,
    country   text
);

CREATE TABLE IF NOT EXISTS public.publisher (
    publisher_id integer PRIMARY KEY,
    name         text NOT NULL
);

CREATE TABLE IF NOT EXISTS public.customer (
    customer_id integer PRIMARY KEY,
    email       text NOT NULL,
    full_name   text
);

CREATE TABLE IF NOT EXISTS public.book (
    book_id      integer PRIMARY KEY,
    title        text NOT NULL,
    author_id    integer NOT NULL REFERENCES public.author (author_id),
    publisher_id integer REFERENCES public.publisher (publisher_id),
    isbn         text,
    price        numeric(10, 2) NOT NULL,
    is_featured  boolean NOT NULL DEFAULT false,
    -- GENERATED column: the injector must never try to write to it.
    price_with_tax numeric(10, 2) GENERATED ALWAYS AS (price * 1.20) STORED,
    -- Deferrable UNIQUE constraint: the injector defers it during the load.
    CONSTRAINT book_isbn_key UNIQUE (isbn) DEFERRABLE INITIALLY IMMEDIATE
);

CREATE TABLE IF NOT EXISTS public.book_order (
    order_id    integer PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES public.customer (customer_id),
    order_date  date NOT NULL
);

CREATE TABLE IF NOT EXISTS public.order_line (
    order_line_id integer PRIMARY KEY,
    order_id      integer NOT NULL REFERENCES public.book_order (order_id),
    book_id       integer NOT NULL REFERENCES public.book (book_id),
    quantity      integer NOT NULL
);

-- Key/value config table, not injected: seeded here and touched via --pre-commit-sql.
CREATE TABLE IF NOT EXISTS public.config (
    config_id integer PRIMARY KEY,
    key       text,
    val       text
);
INSERT INTO public.config (config_id, key, val)
VALUES (666, 'greeting', 'hello')
ON CONFLICT (config_id) DO NOTHING;

-- Derived table, not injected: rebuilt by the post-processing function.
CREATE TABLE IF NOT EXISTS public.sales_summary (
    book_id        integer PRIMARY KEY,
    total_quantity integer NOT NULL
);

-- Post-processing function, invoked via `--pre-commit-sql 'function public.recompute_sales_summary'`.
CREATE OR REPLACE FUNCTION public.recompute_sales_summary() RETURNS void AS $$
BEGIN
    DELETE FROM public.sales_summary;
    INSERT INTO public.sales_summary (book_id, total_quantity)
    SELECT book_id, COALESCE(sum(quantity), 0)
    FROM public.order_line
    GROUP BY book_id;
END;
$$ LANGUAGE plpgsql;

-- Business rule: at most one book can be featured at a time. Injecting two
-- featured books makes this raise (used by the failing test).
CREATE OR REPLACE FUNCTION public.enforce_single_featured_book() RETURNS trigger AS $$
BEGIN
    IF (SELECT count(*) FROM public.book WHERE is_featured) > 1 THEN
        RAISE EXCEPTION 'only one book can be featured';
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS enforce_single_featured_book ON public.book;
CREATE TRIGGER enforce_single_featured_book
    AFTER INSERT OR UPDATE ON public.book
    FOR EACH STATEMENT EXECUTE FUNCTION public.enforce_single_featured_book();
