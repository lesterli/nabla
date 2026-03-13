import { createBrowserRouter } from "react-router-dom";
import Layout from "./components/Layout";
import BriefPage from "./pages/BriefPage";
import ScreeningPage from "./pages/ScreeningPage";
import TopicsPage from "./pages/TopicsPage";

export const router = createBrowserRouter([
  {
    element: <Layout />,
    children: [
      { path: "/", element: <BriefPage /> },
      { path: "/screening", element: <ScreeningPage /> },
      { path: "/topics", element: <TopicsPage /> },
    ],
  },
]);
