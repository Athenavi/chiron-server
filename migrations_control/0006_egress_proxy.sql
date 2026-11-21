-- S7：租户 egress 代理白名单。
-- egress_mode 新增 proxy：沙箱通过服务器侧正向代理出网；allowlist 按租户套餐生效。
-- bridge 仍可直接出网（受 Docker 网络限制）；none 完全断网。

ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS egress_allowlist text[] NOT NULL DEFAULT '{}';

ALTER TABLE plans DROP CONSTRAINT IF EXISTS plans_egress_mode_check;
ALTER TABLE plans
    ADD CONSTRAINT plans_egress_mode_check
    CHECK (egress_mode IN ('bridge', 'proxy', 'none'));

UPDATE plans
   SET egress_allowlist = ARRAY[
        'github.com', 'api.github.com', 'codeload.github.com', '*.githubusercontent.com',
        'pypi.org', 'files.pythonhosted.org',
        'registry.npmjs.org', 'crates.io', 'static.crates.io', 'index.crates.io',
        'deb.debian.org', 'security.debian.org', 'dl-cdn.alpinelinux.org'
       ]
 WHERE cardinality(egress_allowlist) = 0;
