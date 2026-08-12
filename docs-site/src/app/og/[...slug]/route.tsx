import { getPage, getPageImage, getAllPages } from '@/lib/content';
import { ImageResponse } from 'next/og';

export const revalidate = false;

// Neutral ramp drawn from the Tidebreak palette — the site is deliberately
// monochrome, so the card panel is a lightness ramp rather than brand hues.
const GRID_COLORS = [
  ['#d4d4d8', '#a1a1aa', '#3f3f46', '#52525b'],
  ['#e4e4e7', '#52525b', '#18181b', '#a1a1aa'],
  ['#a1a1aa', '#18181b', '#3f3f46', '#d4d4d8'],
  ['#3f3f46', '#d4d4d8', '#a1a1aa', '#27272a'],
];

export async function GET(
  _req: Request,
  { params }: RouteContext<'/og/[...slug]'>,
) {
  const { slug } = await params;
  const pageSlug = slug.slice(0, -1);
  const page = getPage(pageSlug.length === 0 ? undefined : pageSlug);
  // Static export only renders the params below, so a miss here means the
  // scaffold has no content yet — fall back to a bare site card.
  const title = page?.title ?? 'Tidebreak Docs';
  const description = page?.description ?? '';

  return new ImageResponse(
    (
      <div
        style={{
          display: 'flex',
          width: '100%',
          height: '100%',
          backgroundColor: '#ffffff',
        }}
      >
        <div
          style={{
            display: 'flex',
            flexDirection: 'column',
            width: '55%',
            height: '100%',
            padding: '48px 56px',
            justifyContent: 'flex-start',
          }}
        >
          <div
            style={{
              display: 'flex',
              width: '100%',
              height: '2px',
              backgroundColor: '#000000',
              marginBottom: '32px',
            }}
          />
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: '12px',
              marginBottom: '32px',
            }}
          >
            <span
              style={{ fontSize: '28px', fontWeight: 700, color: '#000000' }}
            >
              Tidebreak
            </span>
            <span
              style={{ fontSize: '28px', fontWeight: 400, color: '#52525b' }}
            >
              Docs
            </span>
          </div>
          <div
            style={{
              display: 'flex',
              fontSize: '56px',
              fontWeight: 700,
              color: '#000000',
              lineHeight: 1.15,
              marginBottom: '24px',
            }}
          >
            {title}
          </div>
          {description && (
            <div
              style={{
                display: 'flex',
                fontSize: '22px',
                color: '#374151',
                lineHeight: 1.5,
              }}
            >
              {description}
            </div>
          )}
        </div>
        <div
          style={{
            display: 'flex',
            flexDirection: 'column',
            width: '45%',
            height: '100%',
          }}
        >
          {GRID_COLORS.map((row, i) => (
            <div
              key={i}
              style={{
                display: 'flex',
                width: '100%',
                height: '25%',
              }}
            >
              {row.map((color, j) => (
                <div
                  key={j}
                  style={{
                    display: 'flex',
                    width: '25%',
                    height: '100%',
                    backgroundColor: color,
                  }}
                />
              ))}
            </div>
          ))}
        </div>
      </div>
    ),
    {
      width: 1200,
      height: 630,
    },
  );
}

export function generateStaticParams() {
  const pages = getAllPages();
  // `output: export` requires at least one param, so the empty-content
  // scaffold still emits the bare site card.
  if (pages.length === 0) return [{ slug: ['image.png'] }];

  return pages.map((page) => ({
    slug: getPageImage(page).segments,
  }));
}
