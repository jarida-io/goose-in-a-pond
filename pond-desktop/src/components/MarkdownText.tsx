import { Fragment, useId, useMemo, type ReactNode } from "react";
import { Check } from "lucide-react";
import { blockId, parseMarkdown, type Align, type Block, type Inline, type Tasks } from "../lib/markdownBlocks";
import "./markdown.css";

/** Heading tags by level: a chat bubble sits below the page's own headings, a report has only its title above them. */
const HEADING_TAGS = {
  chat: ["h4", "h4", "h5", "h6", "h6", "h6"],
  report: ["h3", "h3", "h4", "h5", "h5", "h5"],
} as const;

export type HeadingStyle = keyof typeof HEADING_TAGS;

interface Ctx {
  headings: HeadingStyle;
  /** Set when a heading must be findable by id; only top-level blocks get one. */
  idPrefix?: string;
}

function Inlines({ nodes }: { nodes: Inline[] }) {
  return (
    <>
      {nodes.map((node, i): ReactNode => {
        switch (node.t) {
          case "text":
            return <Fragment key={i}>{node.v}</Fragment>;
          case "code":
            return (
              <code key={i} className="md__code">
                {node.v}
              </code>
            );
          case "strong":
            return (
              <strong key={i}>
                <Inlines nodes={node.c} />
              </strong>
            );
          case "em":
            return (
              <em key={i}>
                <Inlines nodes={node.c} />
              </em>
            );
          case "del":
            return (
              <del key={i}>
                <Inlines nodes={node.c} />
              </del>
            );
          case "link":
            return (
              <a key={i} href={node.href} target="_blank" rel="noopener noreferrer">
                <Inlines nodes={node.c} />
              </a>
            );
        }
      })}
    </>
  );
}

/** A task item: a read-only box, named by the item's own words, then those words. */
function TaskItem({ checked, children }: { checked: boolean; children: ReactNode }) {
  const id = useId();
  return (
    <li className="md__task">
      <span className="md__box" role="checkbox" aria-checked={checked} aria-readonly="true" aria-labelledby={id}>
        {checked ? <Check size={12} strokeWidth={3} aria-hidden="true" /> : null}
      </span>
      <div id={id} className="md__taskText" data-done={checked ? "true" : undefined}>
        {children}
      </div>
    </li>
  );
}

function Items({ items, tasks, ctx }: { items: Block[][]; tasks?: Tasks; ctx: Ctx }) {
  return (
    <>
      {items.map((item, j) => {
        const done = tasks?.[j];
        const body = <Blocks blocks={item} tight ctx={{ ...ctx, idPrefix: undefined }} />;
        return typeof done === "boolean" ? (
          <TaskItem key={j} checked={done}>
            {body}
          </TaskItem>
        ) : (
          <li key={j}>{body}</li>
        );
      })}
    </>
  );
}

const ALIGN_CLASS: Record<"left" | "center" | "right", string> = {
  left: " md__cell--left",
  center: " md__cell--center",
  right: " md__cell--right",
};

function alignClass(align: Align): string {
  return align === null ? "" : ALIGN_CLASS[align];
}

function Blocks({ blocks, tight, ctx }: { blocks: Block[]; tight?: boolean; ctx: Ctx }) {
  // A list item holding one paragraph is drawn without the paragraph's margins.
  if (tight && blocks.length === 1 && blocks[0].t === "p") return <Inlines nodes={blocks[0].c} />;
  return (
    <>
      {blocks.map((block, i): ReactNode => {
        switch (block.t) {
          case "p":
            return (
              <p key={i} className="md__p">
                <Inlines nodes={block.c} />
              </p>
            );
          case "h": {
            const Tag = HEADING_TAGS[ctx.headings][Math.min(block.level, 6) - 1];
            return (
              <Tag
                key={i}
                className={`md__h md__h${block.level}`}
                id={ctx.idPrefix === undefined ? undefined : blockId(ctx.idPrefix, i)}
                tabIndex={ctx.idPrefix === undefined ? undefined : -1}
              >
                <Inlines nodes={block.c} />
              </Tag>
            );
          }
          case "ul":
            return (
              <ul key={i} className="md__list">
                <Items items={block.items} tasks={block.tasks} ctx={ctx} />
              </ul>
            );
          case "ol":
            return (
              <ol key={i} className="md__list" start={block.start}>
                <Items items={block.items} tasks={block.tasks} ctx={ctx} />
              </ol>
            );
          case "table":
            return (
              <div key={i} className="md__tablewrap" role="region" aria-label="Table" tabIndex={0}>
                <table className="md__table">
                  <thead>
                    <tr>
                      {block.head.map((cell, col) => (
                        <th key={col} scope="col" className={`md__cell${alignClass(block.align[col])}`}>
                          <Inlines nodes={cell} />
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {block.rows.map((row, r) => (
                      <tr key={r}>
                        {row.map((cell, col) => (
                          <td key={col} className={`md__cell${alignClass(block.align[col])}`}>
                            <Inlines nodes={cell} />
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          case "quote":
            return (
              <blockquote key={i} className="md__quote">
                <Blocks blocks={block.c} ctx={{ ...ctx, idPrefix: undefined }} />
              </blockquote>
            );
          case "code":
            return (
              <pre key={i} className="md__pre">
                <code>{block.v}</code>
              </pre>
            );
          case "hr":
            return <hr key={i} className="md__hr" />;
        }
      })}
    </>
  );
}

interface Props {
  source: string;
  className?: string;
  /** `report` for a long document whose own title is the page's heading; `chat` (the default) for a bubble. */
  headings?: HeadingStyle;
  /** Gives each top-level heading an id (`blockId(prefix, index)`) and makes it focusable, so an outline can land on it. */
  idPrefix?: string;
}

/** Draws markdown the pond wrote as elements, never as HTML, so nothing in it can run. */
export function MarkdownText({ source, className, headings = "chat", idPrefix }: Props) {
  const blocks = useMemo(() => parseMarkdown(source), [source]);
  return (
    <div className={`md${className ? ` ${className}` : ""}`}>
      <Blocks blocks={blocks} ctx={{ headings, idPrefix }} />
    </div>
  );
}
