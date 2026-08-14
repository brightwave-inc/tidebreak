import { Geist, Geist_Mono } from 'next/font/google';
import { Provider } from '@/components/provider';
import type { Metadata } from 'next';
import { getPage, getPageImage } from '@/lib/content';
import { PRODUCT_URL } from '@/lib/site';
import './global.css';

const geistSans = Geist({
  subsets: ['latin'],
  variable: '--font-geist-sans',
});

const geistMono = Geist_Mono({
  subsets: ['latin'],
  variable: '--font-geist-mono',
});

const homePage = getPage([]);
const homeImage = homePage ? getPageImage(homePage).url : '/og/image.png';

export const metadata: Metadata = {
  metadataBase: new URL(PRODUCT_URL),
  title: {
    default: 'Tidebreak Docs',
    template: '%s — Tidebreak Docs',
  },
  description: 'Documentation for Tidebreak.',
  openGraph: {
    siteName: 'Tidebreak Docs',
    images: homeImage,
  },
  twitter: {
    card: 'summary_large_image',
    images: homeImage,
  },
};

export default function Layout({ children }: LayoutProps<'/'>) {
  return (
    <html
      lang="en"
      className={`${geistSans.variable} ${geistMono.variable}`}
      data-scroll-behavior="smooth"
      suppressHydrationWarning
    >
      <body className="flex flex-col min-h-screen">
        <Provider>{children}</Provider>
      </body>
    </html>
  );
}
