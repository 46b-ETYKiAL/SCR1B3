<?xml version="1.0" encoding="utf-8"?>
<!--
  heat(1) transform: give every harvested license directory a RemoveFolder row.

  `heat dir` emits Directory/Component/File elements but NEVER a RemoveFolder,
  so a harvested tree leaves its directories behind on uninstall. For SCR1B3
  that is not merely untidy: the install root is inside the user profile
  (%LOCALAPPDATA%\Programs\SCR1B3), and ICE64 fails the link for every
  user-profile directory that is absent from the RemoveFile table. Harvesting
  the 22-font license tree adds 25 such directories, so a raw heat fragment
  fails `light` with 25 x LGHT0204/ICE64.

  The fix is the SAME one main.wxs already applies to APPLICATIONFOLDER and
  ProgramsFolder: author the rows, so ICE64 stays ENFORCED. Suppressing it with
  -sice:ICE64 would "work" and would throw away the check that catches the next
  uncleaned directory — the opposite of what that comment in main.wxs says was
  deliberately bought back.

  Removal is empty-only, so a directory holding anything the user or the in-app
  updater left behind still survives (verified with a foreign-occupant
  install/uninstall probe).

  One RemoveFolder per Component rather than per Directory: several Components
  can share a Directory, and duplicate rows for one directory are legal (they
  are keyed by their own Id). Per-Component keeps the generated Id derivable
  from the Component Id and therefore unique and stable.
-->
<xsl:stylesheet version="1.0"
                xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
                xmlns:wix="http://schemas.microsoft.com/wix/2006/wi"
                xmlns="http://schemas.microsoft.com/wix/2006/wi"
                exclude-result-prefixes="wix">

  <xsl:output method="xml" indent="yes" />

  <!-- Identity: copy everything heat produced, unchanged. -->
  <xsl:template match="@*|node()">
    <xsl:copy>
      <xsl:apply-templates select="@*|node()" />
    </xsl:copy>
  </xsl:template>

  <!--
    Only Components nested in a harvested <Directory>. Components sitting
    directly under the <DirectoryRef> are in APPLICATIONFOLDER, which main.wxs
    already removes explicitly.

    Every ANCESTOR Directory is covered, not just the immediate parent. An
    intermediate directory can hold no files of its own — `licenses/fonts`
    contains only the 22 per-font subdirectories — so it gets no Component, and
    a parent-only rule leaves exactly that one directory unlisted. Measured, not
    predicted: the parent-only version of this transform emitted 24 rows and
    `light` still failed with one ICE64, for `licenses/fonts`.

    Duplicate rows for one directory are legal (each is keyed by its own Id) and
    Windows Installer removes a directory only once, and only when empty.
  -->
  <xsl:template match="wix:Component[parent::wix:Directory]">
    <xsl:variable name="cmp" select="substring(@Id, 4)" />
    <xsl:copy>
      <xsl:apply-templates select="@*|node()" />
      <xsl:for-each select="ancestor::wix:Directory">
        <RemoveFolder On="uninstall">
          <!-- heat Ids are `cmp<32 hex>`; `rmf<32 hex>_<n>` is unique per
               (component, ancestor) pair and well under the 72-char
               Identifier limit. -->
          <xsl:attribute name="Id">
            <xsl:value-of select="concat('rmf', $cmp, '_', position())" />
          </xsl:attribute>
          <xsl:attribute name="Directory">
            <xsl:value-of select="@Id" />
          </xsl:attribute>
        </RemoveFolder>
      </xsl:for-each>
    </xsl:copy>
  </xsl:template>

</xsl:stylesheet>
