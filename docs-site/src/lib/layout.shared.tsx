import type { BaseLayoutProps } from 'fumadocs-ui/layouts/shared';
import { TidebreakLogo } from '@/components/logo';
import {
  DOWNLOAD_URL,
  PRODUCT_URL,
  REPO_URL,
  ROADMAP_URL,
} from '@/lib/site';

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: <TidebreakLogo />,
      url: '/',
    },
    links: [
      {
        text: 'Product',
        url: PRODUCT_URL,
        external: true,
      },
      {
        text: 'Roadmap',
        url: ROADMAP_URL,
        external: true,
      },
      {
        type: 'button',
        text: 'Download',
        url: DOWNLOAD_URL,
        external: true,
      },
    ],
    githubUrl: REPO_URL,
  };
}
