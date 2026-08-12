import { Inter } from 'next/font/google';
import { Provider } from '@/components/provider';
import type { Metadata } from 'next';
import { getPage, getPageImage } from '@/lib/content';
import './global.css';

const inter = Inter({
  subsets: ['latin'],
});

const homePage = getPage([]);
const homeImage = homePage ? getPageImage(homePage).url : '/og/image.png';

export const metadata: Metadata = {
  metadataBase: new URL('https://tidebreak.dev'),
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
    <html lang="en" className={inter.className} suppressHydrationWarning>
      <body className="flex flex-col min-h-screen">
        <Provider>{children}</Provider>
      </body>
    </html>
  );
}
