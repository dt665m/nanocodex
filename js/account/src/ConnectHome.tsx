import { PhoneService } from "./PhoneService";
import { AccountMenu } from "./AccountMenu";
import { NavLink, useLocation } from "react-router";
import { Vault } from "./Vault";
import { RemoteScreens } from "./RemoteScreens";
import { useAccountSession } from "./AccountSession";

export function ConnectHome() {
  const location = useLocation();
  const { account } = useAccountSession();
  const phone = location.pathname.replace(/\/+$/, "") === "/services/phone";
  const vault = ["/connect/vault", "/vault"].includes(location.pathname.replace(/\/+$/, ""));
  return (
    <div className="device-connect-route connect-home" data-testid="connect-home">
      <section className="connect-wizard">
        <nav className="connect-home-navigation" aria-label="Connect settings">
          <NavLink end to="/connect">Connect</NavLink>
          <NavLink to="/connect/vault">Vault</NavLink>
          <NavLink to="/services/phone">Phone numbers</NavLink>
          {account?.persistent && <RemoteScreens key={account.id} showLabel />}
        </nav>
        <div className="wizard-content">
          {phone ? <PhoneService /> : vault ? <Vault key={account?.id ?? "signed-out"} /> : <AccountMenu inline />}
        </div>
      </section>
    </div>
  );
}
