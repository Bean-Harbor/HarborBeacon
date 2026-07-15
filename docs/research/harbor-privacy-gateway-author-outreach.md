# Harbor Privacy Gateway Author Outreach

Last updated: 2026-06-10

## Outreach原则

- 不把论文作者当作已经合作的外部背书。
- 第一封邮件只表达研究兴趣和具体合作方向，不附带投资话术。
- 优先联系论文明确标注的通讯作者；如果没有明确通讯作者，就联系 arXiv submitter / 项目页公开邮箱 / 同团队公开邮箱。
- 目标不是“求背书”，而是争取 benchmark 复现、场景共建、学生项目、顾问或联合技术报告的可能性。

## Contact Map

| Priority | Paper | Contact | Evidence | Suggested ask |
| --- | --- | --- | --- | --- |
| P0 | SopriBench / Argus, `What Your Posts Reveal` | Xinlei He `<xinlei.he@whu.edu.cn>`; Jiaheng Wei `<jiahengwei@hkust-gz.edu.cn>` | arXiv HTML explicitly lists them as co-corresponding authors. | Adapt SopriBench/PES to home multimodal timeline privacy; discuss synthetic benchmark and gateway evaluation. |
| P1 | VoxPrivacy | Yuxiang Wang / Zhizheng Wu group; use `<yuxiangwang1@link.cuhk.edu.cn>` and `<wuzhizheng@cuhk.edu.cn>` from related VoxSafeBench page if no better email is found. | VoxPrivacy arXiv lists CUHK-Shenzhen authors but no public email in source; VoxSafeBench is overlapping group and publishes emails. | Multi-user voice privacy and speaker-aware access control for home assistants. |
| P1 | VoxSafeBench | Yuxiang Wang `<yuxiangwang1@link.cuhk.edu.cn>`; Zhizheng Wu `<wuzhizheng@cuhk.edu.cn>` | arXiv HTML lists both emails. | Audio-conditioned privacy/safety benchmark for Harbor home voice gateway. |
| P2 | PII-Bench | Weili Han `<wlhan@fudan.edu.cn>` | Paper author block marks Weili Han with `*` and gives the email. | Chinese query-aware masking baseline; extend from text PII to multimodal household prompts. |
| P2 | PrivaCI-Bench | Sirui Han `<siruihan@ust.hk>`; Yangqiu Song `<yqsong@cse.ust.hk>` | arXiv HTML marks Sirui Han as corresponding author and lists both emails. | Contextual integrity policy model for Harbor route policy and audit records. |

## First Email Draft: SopriBench / Argus

Subject: Collaboration inquiry: home multimodal privacy gateways inspired by SopriBench/Argus

Dear Dr. He and Dr. Wei,

My name is [Your Name], founder of Harbor Innovations, a Shenzhen-based startup building local AI hardware and software for the home.

We are trying to make AI genuinely useful inside everyday family life: helping people understand what is happening at home, remember important moments, coordinate devices, and interact naturally with their living environment. The hard part is not only model capability. In our view, AI can only enter the home if families can trust how their data is handled. The home is not just another data source. It contains children, elderly parents, visitors, routines, voices, rooms, documents, and many small contextual details that become sensitive when they are connected over time.

That is why your recent paper, *What Your Posts Reveal: A Benchmark and Agentic Framework for User-Level Privacy Leakage on Social Media*, caught my attention. The core observation in the paper is very close to a problem we keep running into: privacy leakage is often cumulative, cross-post, and multimodal, rather than a single obvious PII field.

Our current answer is to explore a local Privacy Gateway before cloud model calls. We do not believe everything can or should stay on-device forever; cloud models will still be useful for complex reasoning. The question is what should be allowed to leave the home. In a real home environment, camera frames, voice, OCR, device states, and timeline events may all be useful for AI tasks, but sending raw multimodal data to the cloud creates obvious privacy risk. Our current direction is to convert local signals into a task-minimal semantic capsule before any cloud inference: only the facts needed for the task should leave the device, with an auditable privacy decision record.

We are still early, so I do not want to overstate our progress. What we have now is a clear product direction, a local device/software architecture, and a strong need for a rigorous evaluation framework. Your SopriBench/PES/Argus framing looks like an excellent research foundation for evaluating whether this kind of gateway actually reduces cumulative leakage while preserving task utility.

Would you be open to a short conversation about possible collaboration? A few concrete directions we have in mind:

1. Adapting SopriBench/PES-style evaluation to synthetic home multimodal timeline scenarios.
2. Building a small benchmark around raw home signals vs. task-minimal semantic capsules.
3. Evaluating whether gateway transformations reduce inferred sensitive attributes while preserving answer quality.
4. Exploring a student project, research internship, sponsored research, or joint technical report if there is a good fit.

We can prepare a short one-page overview before the call. We are especially interested in doing this in a careful way: synthetic data first, no real household data release, and no claims of collaboration unless both sides agree.

Would you have 30 minutes sometime in the next two weeks?

Best regards,

[Your Name]

Harbor Innovations

[Phone / WeChat]
[Email]
