// The last word when the page itself fails to draw: on Windows the window has
// no frame of the system's (P15), so the screen it shows then keeps the
// window's own title bar, to move and close it.
import { Component, type ReactNode } from "react";
import { Fatal } from "./Screens";
import { TitleBar } from "./TitleBar";
import "./App.css";

export class Boundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state: { error: string | null } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error.message : String(error) };
  }

  render() {
    if (this.state.error == null) return this.props.children;
    return (
      <div className="window">
        <TitleBar />
        <Fatal error={this.state.error} />
      </div>
    );
  }
}
