import { NavLink, Outlet } from "react-router-dom";

const tabs = [
  { to: "/", label: "Brief" },
  { to: "/papers", label: "Papers" },
  { to: "/screening", label: "Screening" },
  { to: "/topics", label: "Topics" },
];

export default function Layout() {
  return (
    <div className="min-h-screen">
      <nav className="border-b bg-white px-6 py-3 flex items-center gap-6">
        <span className="font-semibold text-lg tracking-tight">nabla</span>
        {tabs.map((tab) => (
          <NavLink
            key={tab.to}
            to={tab.to}
            className={({ isActive }) =>
              `text-sm px-2 py-1 rounded ${isActive ? "bg-gray-900 text-white" : "text-gray-600 hover:text-gray-900"}`
            }
          >
            {tab.label}
          </NavLink>
        ))}
      </nav>
      <main className="max-w-5xl mx-auto px-6 py-8">
        <Outlet />
      </main>
    </div>
  );
}
