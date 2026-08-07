/** @type {import('next').NextConfig} */
const config = {
  output: 'export',
  trailingSlash: true,
  reactStrictMode: true,
  images: { unoptimized: true },
  ...(process.env.BASE_PATH && { basePath: process.env.BASE_PATH }),
};

export default config;
