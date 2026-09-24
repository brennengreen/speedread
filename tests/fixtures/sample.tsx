import React, { forwardRef } from "react";

type Props = { title: string };

export function Header({ title }: Props) {
  const [open, setOpen] = React.useState(false);
  return (
    <header onClick={() => setOpen(!open)}>
      <h1>{title}</h1>
    </header>
  );
}

export const Button = forwardRef<HTMLButtonElement, Props>((props, ref) => {
  const cls = "btn";
  return <button ref={ref} className={cls}>{props.title}</button>;
});
