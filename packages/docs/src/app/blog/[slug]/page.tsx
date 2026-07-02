import { blogSource } from '@/lib/source';
import { notFound } from 'next/navigation';
import defaultMdxComponents from 'fumadocs-ui/mdx';
import { Mermaid } from '@/components/mermaid';
import { VaporwaveGrid } from '@/components/vaporwave-grid';
import { getSequoiaPublicationUri } from '@/lib/sequoia';
import Image from 'next/image';

export default async function BlogPost(props: {
  params: Promise<{ slug: string }>;
}) {
  const params = await props.params;
  const page = blogSource.getPage([params.slug]);

  if (!page) notFound();

  const { title, description, date, author, tags, atUri } = page.data;
  const Mdx = page.data.body;
  const publicationUri = getSequoiaPublicationUri();

  return (
    <article className="mx-auto max-w-3xl px-6 py-16" style={{ isolation: 'isolate' }}>
      {publicationUri && (
        <link rel="site.standard.publication" href={publicationUri} />
      )}
      {atUri && (
        <link rel="site.standard.document" href={atUri} />
      )}
      <header className="mb-12">
        <h1 className="text-4xl font-bold mb-4">{title}</h1>
        {description && (
          <p className="text-lg" style={{ color: 'rgb(var(--color-fg-muted))' }}>
            {description}
          </p>
        )}
        <div className="mt-6 flex flex-wrap items-center gap-4">
          <div className="flex items-center gap-2">
            <Image
              src={author.avatar}
              alt={author.name}
              width={24}
              height={24}
              className="rounded-full"
            />
            <span className="text-sm" style={{ color: 'rgb(var(--color-fg-muted))' }}>
              {author.name}
            </span>
          </div>
          <time
            dateTime={date.toISOString()}
            className="text-sm"
            style={{ color: 'rgb(var(--color-fg-muted))' }}
          >
            {date.toLocaleDateString('en-US', {
              year: 'numeric',
              month: 'long',
              day: 'numeric',
            })}
          </time>
          {tags && tags.length > 0 && (
            <div className="flex flex-wrap gap-2">
              {tags.map((tag) => (
                <span
                  key={tag}
                  className="rounded-full px-2.5 py-0.5 text-xs font-medium"
                  style={{
                    backgroundColor: 'rgb(var(--color-magenta) / 0.12)',
                    color: 'rgb(var(--color-magenta))',
                  }}
                >
                  {tag}
                </span>
              ))}
            </div>
          )}
        </div>
      </header>
      <div className="prose prose-invert max-w-none">
        <Mdx components={{ ...defaultMdxComponents, Mermaid }} />
      </div>
      <VaporwaveGrid />
    </article>
  );
}

export function generateStaticParams() {
  return blogSource.generateParams().map((params) => ({
    slug: params.slug?.[0] ?? '',
  }));
}

export async function generateMetadata(props: {
  params: Promise<{ slug: string }>;
}) {
  const params = await props.params;
  const page = blogSource.getPage([params.slug]);
  if (!page) return {};

  return {
    title: page.data.title,
    description: page.data.description,
  };
}
