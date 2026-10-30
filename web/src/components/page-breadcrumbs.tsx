import { Fragment } from "react";
import Link from "next/link";

import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb";

export type Crumb = {
  label: string;
  /** Omitted on the last crumb, which is where the reader already is. */
  href?: string;
};

/**
 * The trail above a page's content. A back button says only "somewhere else";
 * a trail says where you are and what contains it, which is the question a
 * reader two levels deep is actually asking.
 */
export function PageBreadcrumbs({ crumbs }: { crumbs: Crumb[] }) {
  if (crumbs.length === 0) return null;

  return (
    <Breadcrumb className="px-4 pt-4 md:px-6">
      <BreadcrumbList>
        {crumbs.map((crumb, index) => {
          const last = index === crumbs.length - 1;
          // The separator is its own <li>, so it sits beside the item rather
          // than inside it.
          return (
            <Fragment key={`${crumb.label}-${index}`}>
              <BreadcrumbItem>
                {last || !crumb.href ? (
                  <BreadcrumbPage>{crumb.label}</BreadcrumbPage>
                ) : (
                  <BreadcrumbLink asChild>
                    <Link href={crumb.href}>{crumb.label}</Link>
                  </BreadcrumbLink>
                )}
              </BreadcrumbItem>
              {last ? null : <BreadcrumbSeparator />}
            </Fragment>
          );
        })}
      </BreadcrumbList>
    </Breadcrumb>
  );
}
