import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CreateSpaceModal } from "../../src/features/spaces/CreateSpaceModal.tsx";
import { nameToSafeSlug } from "../../src/features/spaces/spaceMapping.ts";

describe("CreateSpaceModal", () => {
  it("renders space name and slug input fields", () => {
    render(
      <CreateSpaceModal
        open={true}
        name="神秘图片"
        slug="shen_mi_tu_pian"
        error=""
        busy={false}
        onChangeName={vi.fn()}
        onChangeSlug={vi.fn()}
        onClose={vi.fn()}
        onSubmit={vi.fn()}
      />
    );

    expect(screen.getByRole("heading", { name: "创建空间" })).toBeInTheDocument();
    expect(screen.getByDisplayValue("神秘图片")).toBeInTheDocument();
    expect(screen.getByDisplayValue("shen_mi_tu_pian")).toBeInTheDocument();
    expect(
      screen.getByText(/远端 ModelScope 数据集仓库名称/)
    ).toBeInTheDocument();
  });

  it("triggers onChangeName and onChangeSlug callbacks on input", () => {
    const onChangeName = vi.fn();
    const onChangeSlug = vi.fn();

    render(
      <CreateSpaceModal
        open={true}
        name="test"
        slug="test_slug"
        error=""
        busy={false}
        onChangeName={onChangeName}
        onChangeSlug={onChangeSlug}
        onClose={vi.fn()}
        onSubmit={vi.fn()}
      />
    );

    const nameInput = screen.getByDisplayValue("test");
    fireEvent.change(nameInput, { target: { value: "new name" } });
    expect(onChangeName).toHaveBeenCalledWith("new name");

    const slugInput = screen.getByDisplayValue("test_slug");
    fireEvent.change(slugInput, { target: { value: "custom_slug" } });
    expect(onChangeSlug).toHaveBeenCalledWith("custom_slug");
  });

  it("displays error message when provided", () => {
    render(
      <CreateSpaceModal
        open={true}
        name="test"
        slug="test"
        error="仓库标识必须以小写字母开头"
        busy={false}
        onChangeName={vi.fn()}
        onChangeSlug={vi.fn()}
        onClose={vi.fn()}
        onSubmit={vi.fn()}
      />
    );

    expect(screen.getByText("仓库标识必须以小写字母开头")).toBeInTheDocument();
  });

  it("correctly derives slug for newly supported Chinese terms", () => {
    expect(nameToSafeSlug("神秘图片")).toBe("shen_mi_tu_pian");
    expect(nameToSafeSlug("智能语音问答")).toBe("zhi_neng_yu_yin_wen_da");
  });
});
