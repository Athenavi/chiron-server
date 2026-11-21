/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // 允许联调时用独立产物目录，避免与正在运行的 dev server 争用 `.next`。
  distDir: process.env.NEXT_DIST_DIR || '.next',
  // 前台同源调用 /api/v1（Route Handler 反代到 Rust 服务），不使用 standalone 产物。
  poweredByHeader: false,
  async headers() {
    return [
      {
        source: '/:path*',
        headers: [
          {
            // 不限制脚本/连接，先封住点击劫持、<base> 劫持和表单外投。
            key: 'Content-Security-Policy',
            value: "base-uri 'self'; object-src 'none'; frame-ancestors 'none'; form-action 'self'",
          },
          { key: 'X-Content-Type-Options', value: 'nosniff' },
          { key: 'Referrer-Policy', value: 'no-referrer' },
          { key: 'Permissions-Policy', value: 'camera=(), microphone=(), geolocation=()' },
        ],
      },
    ]
  },
}

export default nextConfig
