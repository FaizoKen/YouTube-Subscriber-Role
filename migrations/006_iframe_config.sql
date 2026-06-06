-- Iframe UI mode + DNF rule engine.
--
-- Adds optimistic-lock version + the rule_tree (OR-of-AND DNF) to role_links,
-- backfilling it from the legacy channel_id + flat `conditions` so existing
-- configured links keep granting the exact same members. The old `conditions`
-- column is intentionally kept (not dropped) so a rollback to the previous
-- binary still works.

ALTER TABLE role_links ADD COLUMN IF NOT EXISTS config_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE role_links ADD COLUMN IF NOT EXISTS rule_tree JSONB NOT NULL
    DEFAULT '{"grant_on_any":false,"groups":[]}';

-- Backfill: a previously-configured link required "is subscribed" AND its flat
-- conditions. Express that as one OR-group: [isSubscribed, ...conditions].
-- Field key `field` -> `target`; everything else is copied verbatim.
UPDATE role_links
SET rule_tree = jsonb_build_object(
    'grant_on_any', false,
    'groups', jsonb_build_array(
        jsonb_build_object(
            'conditions',
            '[{"target":"isSubscribed","operator":"eq","value":true}]'::jsonb ||
            COALESCE((
                SELECT jsonb_agg(
                    jsonb_strip_nulls(jsonb_build_object(
                        'target', c->>'field',
                        'operator', c->>'operator',
                        'value', c->'value',
                        'value_end', c->'value_end'
                    ))
                )
                FROM jsonb_array_elements(conditions) c
            ), '[]'::jsonb)
        )
    )
)
WHERE channel_id IS NOT NULL
  AND rule_tree = '{"grant_on_any":false,"groups":[]}'::jsonb;

-- Allow disabling the public subscribers list (widen the CHECK from 004).
ALTER TABLE guild_settings DROP CONSTRAINT IF EXISTS guild_settings_view_permission_check;
ALTER TABLE guild_settings ADD CONSTRAINT guild_settings_view_permission_check
    CHECK (view_permission IN ('members', 'managers', 'disabled'));
