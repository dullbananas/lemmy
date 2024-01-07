CREATE FUNCTION reverse_timestamp_sort (t timestamp with time zone)
    RETURNS bigint
    AS $$
BEGIN
    RETURN (-1000000 * EXTRACT(EPOCH FROM t))::bigint;
END;
$$
LANGUAGE plpgsql
IMMUTABLE PARALLEL SAFE;

CREATE INDEX idx_post_aggregates_community_published_asc ON public.post_aggregates USING btree (community_id, featured_local DESC, reverse_timestamp_sort (published) DESC);

