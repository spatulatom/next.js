import Nav from "../../components/Nav";
import { NextPage } from "next";
import React from "react";

const News: React.FC = () => (
  <>
    <Nav />
    <p>Hello, I'm the news page</p>
  </>
);

export default News as NextPage;